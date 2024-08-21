extern crate clap;
extern crate reqwest;
extern crate rexiv2;
extern crate serde_json;
use clap::Parser;
use config::Config;
use filetime::{FileTime, set_file_mtime};
use ignore::Walk;
use reqwest::blocking::Client;
use rexiv2::Metadata;
use std::collections::{BTreeMap, HashSet};
use std::error::Error;
use std::ffi::OsStr;
use std::fs::{self, File};
use std::io;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::result::Result;
use std::thread;
use std::time;

#[derive(Clone, Default, Debug)]
struct WITTable {
    btreetable: BTreeMap<String, u8>,
}

impl WITTable {
    pub fn new(hashtable: BTreeMap<String, u8>) -> WITTable {
        WITTable { btreetable: hashtable }
    }

    pub fn push(&mut self, filename: &str, cnt: u8) {
        self.btreetable.insert(filename.to_string(), cnt);
    }

    pub fn pop(&mut self) -> Option<(String, u8)> {
        // traverse the table, pick the image with minimum cnt
        if self.is_empty() {
            None
        } else {
            let mut min_cnt = u8::MAX;
            let mut image_candidate = String::new();
            for (image, &cnt) in &self.btreetable {
                if cnt < min_cnt {
                    min_cnt = cnt;
                    image_candidate = image.clone();
                }
            }
            // get the first element from the minimum-cnt image candidates
            self.btreetable.remove(&image_candidate);
            Some((image_candidate, min_cnt))
        }
    }

    pub fn is_empty(&self) -> bool {
        self.btreetable.is_empty()
    }

    pub fn len(&self) -> usize {
        self.btreetable.len()
    }

    pub fn contains(&self, filename: &str) -> bool {
        self.btreetable.contains_key(filename)
    }

    pub fn decrease_all_cnt(&mut self) {
        // decrease all value by 1 in the btreemap
        for (_, cnt) in self.btreetable.iter_mut() {
            assert!(cnt > &mut 0);
            *cnt -= 1;
        }
    }
}

struct WITConfig {
    album_path: String,
    table_path: String,
    similarity_threshold: f64,
    preserve_quota_percent: f64,
    rescan_interval_minutes: u64,
    flushtable_imgnum: u64,
    saucenao_query_url: String,
}

fn parse_config_from_file(config_path: &str) -> Result<WITConfig, Box<dyn Error>> {
    let config_builder = Config::builder()
        .set_default("table_path", "./table.json")?
        .set_default("album_path", "./")?
        .set_default("api_key", "")?
        .set_default("similarity_threshold", 55)?
        .set_default("preserve_quota_percent", 25)?
        .set_default("rescan_interval_minutes", 5)?
        .set_default("flushtable_imgnum", 3)?
        .add_source(config::File::new(config_path, config::FileFormat::Json))
        .build()?;
    let album_path = config_builder.get_string("album_path")?;
    let table_path = config_builder.get_string("table_path")?;
    let api_key = config_builder.get_string("api_key")?;
    let similarity_threshold = config_builder.get_float("similarity_threshold")?;
    let preserve_quota_percent = config_builder.get_float("preserve_quota_percent")?;
    let rescan_interval_minutes: u64 = config_builder
        .get_int("rescan_interval_minutes")?
        .try_into()?;
    let flushtable_imgnum: u64 = config_builder.get_int("flushtable_imgnum")?.try_into()?;
    let saucenao_query_url = format!(
        "https://saucenao.com/search.php?output_type=2&dbmask=16777216&numres=1&api_key={}",
        api_key
    );
    Ok(WITConfig {
        album_path,
        table_path,
        similarity_threshold,
        preserve_quota_percent,
        rescan_interval_minutes,
        flushtable_imgnum,
        saucenao_query_url,
    })
}

fn get_local_tags(imgpath: &str) -> HashSet<String> {
    match Metadata::new_from_path(imgpath) {
        Ok(metadata) => metadata
            .get_tag_multiple_strings("Xmp.dc.subject")
            .expect("failed to get XMP tag")
            .into_iter()
            .collect::<HashSet<String>>(),
        Err(err) => {
            eprintln!("ERROR on {}, {}", imgpath, err);
            HashSet::<String>::new()
        }
    }
}

fn scan_folder(folder_path: &str, table: &mut WITTable) {
    fn add_to_table(
        abs_path_buf: PathBuf,
        folder_path: &str,
        table: &mut WITTable,
    ) -> Result<(), Box<dyn Error>> {
        // unwrap or default: in case of files with no extension(like.Xrresouces)
        let extension_str = abs_path_buf
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default();
        match extension_str.to_lowercase().as_str() {
            "png" | "jpg" | "bmp" | "jpeg" | "tif" | "tiff"  => {
                let rel_path_str = abs_path_buf
                    .strip_prefix(folder_path)?
                    .to_str()
                    .expect("relative path must be UTF-8");
                if !table.contains(rel_path_str) {
                    let abs_path = abs_path_buf.to_str().expect("absolute path must be UTF-8");
                    let cnt = match get_local_tags(abs_path).is_empty() {
                        true => 0,
                        false => 1,
                    };
                    table.push(rel_path_str, cnt);
                };
            }
            _ => (),
        };
        Ok(())
    }

    fn visit_dirs(folder_path: &str, table: &mut WITTable) -> Result<(), Box<dyn Error>> {
        for result in Walk::new(folder_path) {
            add_to_table(result?.into_path(), folder_path, table)?;
        }
        Ok(())
    }

    match visit_dirs(folder_path, table) {
        Ok(()) => (),
        Err(err) => {
            panic!("Error occured during folder scan, error: {}", err);
        }
    }
}

fn tag_single_image(config: &WITConfig, img_abs_path: &str) -> Result<(i64, i64), io::Error> {
    let file_not_exist_err = || io::Error::new(ErrorKind::NotFound, "File deleted or removed");
    let parse_err = || io::Error::new(ErrorKind::InvalidData, "Failed to parse json");
    let network_err_saucenao = || {
        io::Error::new(
            ErrorKind::NotConnected,
            "Failed to get online tag from saucenao",
        )
    };
    let quota_exceed_err_saucenao =
        || io::Error::new(ErrorKind::PermissionDenied, "Quota exceed for saucenao");
    let network_err_gelbooru = || {
        io::Error::new(
            ErrorKind::NotConnected,
            "Failed to get online tag from gelbooru",
        )
    };

    let rel_path_str = Path::new(img_abs_path)
        .strip_prefix(&config.album_path)
        .unwrap_or(Path::new(""))
        .to_str()
        .unwrap_or_default();
    println!("Image: {}", rel_path_str);
    // check whether the path exists, if not, return an error to remove it from table
    if !Path::new(img_abs_path).exists() {
        return Err(file_not_exist_err());
    }

    // send request to saucenao
    let form = reqwest::blocking::multipart::Form::new().file("file", img_abs_path)?;
    let resp = Client::new()
        .post(&config.saucenao_query_url)
        .multipart(form)
        .send()
        .or(Err(network_err_saucenao()))?;
    // validate the response
    if resp.status().is_server_error() {
        eprintln!("server error!");
        return Err(network_err_saucenao());
    } else if !resp.status().is_success() {
        match resp.status() {
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                println!("No quota left. Waiting for next scan...");
                return Err(quota_exceed_err_saucenao());
            }
            _ => println!("Something else happened. Status: {:?}", resp.status()),
        };
        return Err(network_err_saucenao());
    }

    // parsing result from saucenao

    let resp_json = resp.json::<serde_json::Value>().or(Err(parse_err()))?;
    let short_limit: i64 = resp_json["header"]["short_limit"]
        .as_str()
        .ok_or(parse_err())?
        .parse()
        .or(Err(parse_err()))?;
    let short_remain: i64 = resp_json["header"]["short_remaining"]
        .as_i64()
        .ok_or(parse_err())?;
    let long_limit: i64 = resp_json["header"]["long_limit"]
        .as_str()
        .ok_or(parse_err())?
        .parse()
        .or(Err(parse_err()))?;
    let long_remain: i64 = resp_json["header"]["long_remaining"]
        .as_i64()
        .ok_or(parse_err())?;
    let similarity: f64 = resp_json["results"][0]["header"]["similarity"]
        .as_str()
        .ok_or(parse_err())?
        .parse()
        .or(Err(parse_err()))?;
    // filter similarity
    if similarity <= config.similarity_threshold {
        println!(
            "[Short limit: {}/{}]  Similarity for {} is too low, ignore.",
            short_remain, short_limit, rel_path_str
        );
    } else {
        // parse gelbooru id
        let gelbooru_id: i64 = resp_json["results"][0]["data"]["gelbooru_id"]
            .as_i64()
            .ok_or(parse_err())?;
        // get tags from gelbooru
        let json_url = format!(
            "https://gelbooru.com/index.php?page=dapi&s=post&q=index&json=1&id={}",
            gelbooru_id
        );
        let online_tags = match &reqwest::blocking::get(&json_url)
            .or(Err(network_err_gelbooru()))?
            .json::<serde_json::Value>()
            .or(Err(parse_err()))?["post"][0]["tags"]
        {
            serde_json::Value::Null => {
                println!("failed to deserialize json");
                HashSet::<String>::new()
            }
            v => v
                .to_string()
                .replace("\"", "")
                .split(" ")
                .map(|s| s.to_owned())
                .collect::<HashSet<String>>(),
        };
        // union online tags into local tags
        let local_tags = get_local_tags(img_abs_path);
        if !local_tags.is_superset(&online_tags) {
            // record image mtime before updating tags
            let file_metadata = fs::metadata(img_abs_path).unwrap();
            let mtime = FileTime::from_last_modification_time(&file_metadata);
            {
                // write new tags
                let new_tags = local_tags
                    .union(&online_tags)
                    .into_iter()
                    .map(|x| &**x)
                    .collect::<Vec<&str>>();
                let metadata = Metadata::new_from_path(img_abs_path).expect(&format!(
                    "failed to get metadata from image {}",
                    img_abs_path
                ));
                metadata
                    .set_tag_multiple_strings("Xmp.dc.subject", &new_tags)
                    .expect("Unable to get tags");
                match metadata.save_to_file(img_abs_path) {
                    Err(_) => println!("Failed to save tags for {}", img_abs_path),
                    _ => (),
                };
            };
            // recover mtime back, +1s
            match set_file_mtime(Path::new(img_abs_path), FileTime::from_unix_time(mtime.unix_seconds() + 1, 0)) {
                Err(_) => println!("Failed to set mtime for {}", img_abs_path),
                _ => (),
            };
        };
    }
    // output log
    println!(
        "[Short limit: {}/{}]  Updated {}",
        short_remain, short_limit, rel_path_str
    );
    // sleep for regeneration of short limit
    thread::sleep(time::Duration::from_secs(
        10 / std::cmp::max(1, short_remain) as u64,
    ));
    Ok((long_remain, long_limit))
}

fn tag_all_images(config: &WITConfig, table: &mut WITTable) {
    let mut table_shadow = table.clone();
    let mut entry_to_add_back = Vec::new();
    let mut is_startup = true;
    let mut long_quota: i64 = (&table).len() as i64;
    while long_quota > 0 && !table.is_empty() {
        match table.pop() {
            Some((img_rel_path, cnt)) => {
                // tag the image
                let img_abs_path = format!("{}{}", &config.album_path, &img_rel_path);
                match tag_single_image(&config, &img_abs_path) {
                    Ok((long_remain, long_limit)) => {
                        if long_remain > 0 && long_limit > 0 {
                            // update available_quota
                            long_quota = long_remain as i64
                                - (long_limit as f64 * config.preserve_quota_percent / 100.0).ceil()
                                    as i64;
                            // if it is running for the first time, output some extra info.
                            if is_startup {
                                println!("Long Remaining: {}", long_remain);
                                println!("Long Limit: {}", long_limit);
                                println!("Current quota: {}", long_quota);
                                is_startup = false;
                            }
                        } else {
                            long_quota = 0;
                        }
                    }
                    Err(err) => {
                        match err.kind() {
                            ErrorKind::NotFound => {
                                println!("File {} deleted or removed.", img_abs_path);
                                // file is deleted, we won't add it back into the table
                                continue;
                            }
                            ErrorKind::NotConnected => {
                                println!("{}", err.to_string());
                                long_quota = 0;
                            }
                            ErrorKind::InvalidData => {
                                println!("Failed to parse json from sacenao");
                            }
                            ErrorKind::PermissionDenied => {
                                println!("Saucenao Quota Exceed");
                                long_quota = 0;
                            }
                            _ => {
                                println!("Error: {:?}", err);
                                long_quota = 0;
                            }
                        }
                    }
                }
                // re-push entry into table
                // update table, increase current entry by 1
                // set maximum count to be 4,
                //
                // if the cnt is 4, which means all images reached the maximum cnt 4
                // and no image has cnt==3. We will decrease the count for all images into 3.
                // so that the image with cnt==3 will be tagged again.
                let cnt_new = if cnt >= 4 {
                    table.decrease_all_cnt();
                    4
                } else {
                    cnt + 1
                };
                table_shadow.push(&img_rel_path, cnt_new);
                entry_to_add_back.push((img_rel_path, cnt_new));
            }
            None => {
                eprintln!("Table internal inconsistancy");
                break;
            }
        }
        // write table into disk if idx % cache_num == 0
        if entry_to_add_back.len() as u64 % config.flushtable_imgnum == 0 {
            // save the shadow table
            // 
            // the shadow table contains the entries popped from the main table
            // those entries will be added back to the main table when the epoch ends
            // (quota used up or main table is empty)
            // 
            // so when we interrupt an epoch, the popped entries are still saved in the json file
            // 
            // but the shadow table won't remove the image deleted from the main table,
            // we have to wait for the save for the main table to remove these entries
            save_table(&table_shadow, &config.table_path);
        }
    }
    for (rel_path, cnt) in entry_to_add_back {
        table.push(&rel_path, cnt);
    }
    save_table(&table, &config.table_path);
    // finished one complete scan. wait for next folder scan
    thread::sleep(time::Duration::from_secs(
        60 * config.rescan_interval_minutes,
    )); // long request limit
    scan_folder(&config.album_path, table);
}

fn save_table(table: &WITTable, path: &str) {
    let tmp_path = String::from(path) + "_tmp";
    let table_file = File::create(&tmp_path).expect("Failed to create table file");
    serde_json::to_writer(table_file, &table.btreetable)
        .expect("Failed to serialize table before saving!");
    fs::rename(&tmp_path, &path).expect("unable to save table!");
    let covered = table.btreetable.iter().filter(|(_, &x)| x != 0).count();
    println!(
        "Table Saved!  Images covered: {} / {} ",
        covered,
        table.len()
    );
}

fn read_table(path: &str) -> WITTable {
    fn read_hashtable(path: &str) -> Result<BTreeMap<String, u8>, Box<dyn Error>> {
        let table_file = File::open(path)?;
        let hashtable = serde_json::from_reader(table_file)?;
        Ok(hashtable)
    }

    let hashtable = match read_hashtable(path) {
        Err(e) => {
            eprintln!("{}", e);
            println!("No existing table, create a new table");
            BTreeMap::new()
        }
        Ok(hashtable) => {
            println!("Table loaded! Totally {} images!", hashtable.len());
            hashtable
        }
    };

    WITTable::new(hashtable)
}

#[derive(Parser)]
#[command(name = "Waifu Image Parser")]
#[command(about = "Tag your waifu images with gelbooru data, powered by saucenao")]
struct Cli {
    /// Sets a custom config file
    #[arg(short, long, value_name = "FILE", default_value = "config.json")]
    config: Option<String>,
}

fn main() -> Result<(), Box<dyn Error>> {
    // parsing config
    let cli = Cli::parse();
    let config_path = match cli.config.as_deref() {
        Some(s) => s,
        None => "config.json",
    };
    let config = parse_config_from_file(config_path)?;

    let mut table = read_table(&config.table_path);
    scan_folder(&config.album_path, &mut table);
    save_table(&table, &config.table_path);

    loop {
        tag_all_images(&config, &mut table);
    }
}
