extern crate clap;
extern crate reqwest;
extern crate rexiv2;
extern crate serde_json;
use clap::{arg, App};
use reqwest::blocking::Client;
use rexiv2::Metadata;
use std::collections::{BTreeMap, BTreeSet};
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::result::Result;
use std::thread;
use std::time;

fn get_local_tags(imgpath: &str) -> BTreeSet<String> {
    match Metadata::new_from_path(imgpath.to_owned()) {
        Ok(metadata) => metadata
            .get_tag_multiple_strings("Iptc.Application2.Keywords")
            .expect("failed to get Iptc tag")
            .into_iter()
            .collect::<BTreeSet<String>>(),
        Err(err) => {
            eprintln!("ERROR on {}, {}", imgpath, err);
            BTreeSet::<String>::new()
        }
    }
}

fn scan_folder(folder_path: &str, table: &mut BTreeMap<String, u8>) -> Result<(), Box<dyn Error>> {
    let mut add_to_table = |abs_path_buf: &PathBuf| {
        // unwrap or default: in case of files with no extension(like.Xrresouces)
        let extension = abs_path_buf
            .extension()
            .unwrap_or_default()
            .to_str()
            .unwrap_or_default();
        match extension {
            "png" | "jpg" | "bmp" | "jpeg" | "tif" | "tiff" | "webp" => {
                let rel_path_str = abs_path_buf
                    .strip_prefix(folder_path.clone())
                    .unwrap_or(Path::new(""))
                    .to_str()
                    .unwrap_or_default();
                if !table.contains_key(rel_path_str) {
                    let abs_path = abs_path_buf
                        .to_str()
                        .expect("Path must be UTF-8 and contains no special characters in UTF-16");
                    table.insert(
                        rel_path_str.to_owned(),
                        match get_local_tags(abs_path).is_empty() {
                            true => 0,
                            false => 1,
                        },
                    );
                };
            }
            _ => (),
        };
    };

    fn visit_dirs(dir: &Path, cb: &mut dyn FnMut(&PathBuf)) -> io::Result<()> {
        if dir.is_dir() {
            for entry in fs::read_dir(dir)? {
                let path = entry?.path();
                if path.is_dir() {
                    if path.file_name() != Some(std::ffi::OsStr::new("@eaDir")) {
                        // For synology systems
                        visit_dirs(&path, cb)?;
                    }
                } else {
                    cb(&path);
                }
            }
        }
        Ok(())
    }

    let path = Path::new(folder_path);
    visit_dirs(path, &mut add_to_table).expect("Failed to add images into hashmap");
    Ok(())
}

fn tag_single_image(
    abspath: &str,
    table: &mut BTreeMap<String, u8>,
    url: &str,
    min_similarity: f64,
    album_path: &str,
) -> Result<(i64, i64), Box<dyn Error>> {
    let rel_path_str = Path::new(abspath)
        .strip_prefix(album_path)
        .unwrap_or(Path::new(""))
        .to_str()
        .unwrap_or_default();
    // check whether the path exists, if not, remove it from table
    if !Path::new(abspath).exists() {
        println!("File {} deleted or removed.", abspath);
        table.remove(rel_path_str);
        return Ok((0, 0));
    } else {
        // update table, increase current entry by 1
        match table.get_mut(rel_path_str) {
            Some(val) => {
                *val = if *val != std::u8::MAX { *val + 1 } else { 1 };
            }
            _ => {
                table.insert(rel_path_str.to_owned(), 0);
            }
        }
    }
    // get online tags from saucenao and gelbooru
    let form = reqwest::blocking::multipart::Form::new().file("file", abspath)?;
    let client = Client::new();
    let resp = client.post(url).multipart(form).send()?;
    // validate the response
    if resp.status().is_server_error() {
        println!("server error!");
        return Ok((0, 0));
    } else if !resp.status().is_success() {
        println!("Something else happened. Status: {:?}", resp.status());
        return Ok((0, 0));
    }
    // parsing result from saucenao
    let resp_json = resp.json::<serde_json::Value>()?;
    let short_limit: i64 = resp_json["header"]["short_limit"]
        .as_str()
        .ok_or("parse_err")?
        .parse()?;
    let short_remain: i64 = resp_json["header"]["short_remaining"]
        .as_i64()
        .ok_or("parse_err")?;
    let long_limit: i64 = resp_json["header"]["long_limit"]
        .as_str()
        .ok_or("parse_err")?
        .parse()?;
    let long_remain: i64 = resp_json["header"]["long_remaining"]
        .as_i64()
        .ok_or("parse_err")?;
    let similarity: f64 = resp_json["results"][0]["header"]["similarity"]
        .as_str()
        .ok_or("parse_err")?
        .parse()
        .unwrap_or_default();
    // filter similarity
    if similarity <= min_similarity {
        println!("[Short limit: {}/{}]  Similarity for {} is too low, ignore.", short_remain, short_limit, rel_path_str);
        return Ok((long_remain, long_limit));
    }
    // parse gelbooru id
    let gelbooru_id: i64 = resp_json["results"][0]["data"]["gelbooru_id"]
        .as_i64()
        .ok_or("parse_err")?;
    // get tags from gelbooru
    let json_url = format!(
        "https://gelbooru.com/index.php?page=dapi&s=post&q=index&json=1&id={}",
        gelbooru_id
    );
    let online_tags =
        match &reqwest::blocking::get(&json_url)?.json::<serde_json::Value>()?["post"][0]["tags"] {
            serde_json::Value::Null => {
                println!("failed to deserialize json");
                BTreeSet::<String>::new()
            }
            v => v
                .to_string()
                .replace("\"", "")
                .split(" ")
                .map(|s| s.to_owned())
                .collect::<BTreeSet<String>>(),
        };
    // DEBUG: println
    // println!("online tags: {:?}", online_tags);
    // union online tags into local tags
    let local_tags = get_local_tags(abspath);
    if !local_tags.is_superset(&online_tags) {
        let new_tags = local_tags
            .union(&online_tags)
            .into_iter()
            .map(|x| &**x)
            .collect::<Vec<&str>>();
        let metadata = Metadata::new_from_path(abspath)
            .expect(&format!("failed to get metadata from image {}", abspath));
        metadata
            .set_tag_multiple_strings("Iptc.Application2.Keywords", &new_tags)
            .expect("Unable to get tags");
        match metadata.save_to_file(abspath) {
            Err(_) => println!("Failed to save tags for {}", abspath),
            _ => (),
        };
    }
    // output log
    println!(
        "[Short limit: {}/{}]  Updated {}",
        short_remain, short_limit, rel_path_str
    );
    // sleep for regeneration of short limit
    println!("Thread Sleep for short time limit: {:}s!", 10 / short_remain as u64);
    thread::sleep(time::Duration::from_secs(
        10 / std::cmp::max(1, short_remain) as u64
    ));
    Ok((long_remain, long_limit))
}

fn tag_all_images(
    table: &mut BTreeMap<String, u8>,
    url: &str,
    min_similarity: f64,
    table_path: &str,
    preserve_quota_percent: f64,
    rescan_interval_minutes: u64,
    cache_num: u64,
    album_path: &str,
) {
    let mut running = false;
    let mut long_quota: i64 = std::cmp::min(1, table.len() as i64);
    // sort the table
    let mut vec = table
        .iter()
        .map(|(s, u)| (s.to_owned(), u.to_owned()))
        .collect::<Vec<(String, u8)>>();
    vec.sort_by(|a, b| a.1.partial_cmp(&(b.1)).unwrap());
    // idx
    let idx = 0;
    while long_quota > 0 && vec.len() > idx {
        // in order to get the correct limit, we have to tag an image at first.
        let rel_path = &vec[idx].0;
        let abspath = format!("{}{}", album_path, rel_path);
        match tag_single_image(&abspath, table, url, min_similarity, album_path) {
            Ok((long_remain, long_limit)) => {
                if long_remain > 0 && long_limit > 0 {
                    // update available_quota
                    long_quota = long_remain as i64
                        - (long_limit as f64 * preserve_quota_percent / 100.0).ceil() as i64;
                    // if it is running for the first time, output some extra info.
                    if !running {
                        println!("Long Remaining: {}", long_remain);
                        println!("Long Limit: {}", long_limit);
                        println!("Current quota: {}", long_quota);
                        // shrink vec size if long_quota is smaller than vec.len()
                        if vec.len() as i64 > long_quota {
                            vec.drain(long_quota as usize ..vec.len());
                            vec.shrink_to(long_quota as usize);
                        }
                        running = true;
                    }
                }
            }
            Err(err) => {
                println!("Error: {:?}", err);
            }
        };
        // write table into disk if idx % cache_num == 0
        let idx = idx + 1;
        if 0 == idx as u64 % cache_num {
            save_table(&table, table_path).expect("unable to save table");
        }
    }
    save_table(&table, table_path).expect("unable to save table");
    // finished one complete scan. wait for next folder scan
    thread::sleep(time::Duration::from_secs(60 * rescan_interval_minutes)); // long request limit
    scan_folder(&album_path, table).expect("uanble to rescan the folder");
}

fn save_table(table: &BTreeMap<String, u8>, path: &str) -> io::Result<()> {
    let table_file = File::create(path)?;
    serde_json::to_writer(table_file, &table).expect("Failed to serialize table before saving!");
    let covered = table.iter().filter(|(_, &x)| x != 0).count();
    println!(
        "Table Saved!  Images covered: {} / {} ",
        covered,
        table.len()
    );
    Ok(())
}

fn read_table(table: &mut BTreeMap<String, u8>, path: &str) -> io::Result<()> {
    match File::open(path) {
        Ok(table_file) => {
            let table2: BTreeMap<String, u8> = serde_json::from_reader(table_file)?;
            table.extend(table2);
            println!("Table loaded! Totally {} images!", table.len());
        }
        Err(_) => println!("No existing table, create a new table"),
    }
    Ok(())
}

fn read_config_from_file<P: AsRef<Path>>(path: P) -> Result<serde_json::Value, Box<dyn Error>> {
    let config = serde_json::from_reader(BufReader::new(File::open(path)?))?;
    Ok(config)
}

fn main() -> Result<(), Box<dyn Error>> {
    // parsing config
    let config_path = match App::new("waifu image tagger")
        .args(&[arg!(-c --config <FILE> "set a config file").required(false)])
        .get_matches()
        .value_of("config")
    {
        Some(s) => s.to_owned(),
        None => String::from("config.json"),
    };
    let config = match read_config_from_file(config_path) {
        Ok(config) => config,
        Err(_) => serde_json::json!({
            "table_path": "./table.json",
            "album_path": "./",
            "api_key": "",
            "min_similarity": 55,
            "preserve_quota_percent": 25,
            "rescan_interval_minutes": 5,
            "cache_num": 3
        }),
    };
    let mut table = BTreeMap::<String, u8>::new();
    let album_path = config["album_path"]
        .as_str()
        .expect("album_path must be a string!");
    let table_path = config["table_path"]
        .as_str()
        .expect("table_path must be a string!");
    let api_key = config["api_key"]
        .as_str()
        .expect("api_key must be a string!");
    let min_similarity = config["min_similarity"]
        .as_f64()
        .expect("min_similarity must be a f64 float!");
    let preserve_quota_percent = config["preserve_quota_percent"]
        .as_f64()
        .expect("preserve_quota_percent must be a f64 float!");
    let rescan_interval_minutes = config["rescan_interval_minutes"]
        .as_u64()
        .expect("cache_num must be an u64 integer!");
    let cache_num = config["cache_num"]
        .as_u64()
        .expect("cache_num must be an u64 integer!");
    let url = format!(
        "https://saucenao.com/search.php?output_type=2&dbmask=16777216&numres=1&api_key={}",
        api_key
    );

    read_table(&mut table, table_path).expect("Failed to read table!");
    scan_folder(album_path, &mut table)?;
    save_table(&table, table_path).expect("Unable to save the table");
    loop {
        tag_all_images(
            &mut table,
            &url,
            min_similarity,
            &table_path,
            preserve_quota_percent,
            rescan_interval_minutes,
            cache_num,
            &album_path,
        );
    }
    Ok(())
}
