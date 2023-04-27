extern crate clap;
extern crate reqwest;
extern crate rexiv2;
extern crate serde_json;
use clap::Parser;
use config::Config;
use ignore::Walk;
use reqwest::blocking::Client;
use rexiv2::Metadata;
use std::cmp::Reverse;
use std::collections::{BinaryHeap, HashMap, HashSet};
use std::error::Error;
use std::ffi::OsStr;
use std::fmt::{Binary, Display, Formatter};
use std::fs::{self, File};
use std::io;
use std::io::{ErrorKind};
use std::path::{Path, PathBuf};
use std::result::Result;
use std::thread;
use std::time;

#[derive(Clone, Default, Debug)]
struct Table {
    hashtable: HashMap<String, u8>,
    pq: BinaryHeap<(Reverse<u8>, String)>,
}

impl Table {
    pub fn new(hashtable: HashMap<String, u8>) -> Table {
        let pq = hashtable
            .iter()
            .map(|(s, &u)| (Reverse(u), s.clone()))
            .collect();
        Table { hashtable, pq }
    }

    pub fn push(mut self, filename: &str, cnt: u8) {
        self.hashtable.insert(filename.to_string(), cnt);
        self.pq.push((Reverse(cnt), filename.to_string()));
    }

    pub fn peek(self) {
        self.pq.peek();
    }

    pub fn pop(mut self) -> Option<(String, u8)> {
        let result = self.pq.pop();
        match result {
            Some((cnt, filename)) => {
                self.hashtable.remove(&filename);
                Some((filename, cnt.0))
            },
            _ => {
                None
            }
        }
    }

    pub fn is_empty(self) -> bool {
        self.pq.is_empty()
    }

    pub fn len(self) -> usize {
        self.pq.len()
    }

    pub fn contains(self, filename: &str) -> bool {
        self.hashtable.contains_key(filename)
    }
}


#[derive(Debug)]
enum CustomError {
    InvalidInput,
    FileNotFound,
    IoError(IoError),
}

impl Display for CustomError {
    fn fmt(&self, f: &mut Formatter) -> Result {
        match *self {
            CustomError::InvalidInput => write!(f, "Invalid input"),
            CustomError::FileNotFound => write!(f, "File not found"),
            CustomError::IoError(ref e) => e.fmt(f),
        }
    }
}

impl Error for CustomError {}

impl From<IoError> for CustomError {
    fn from(error: IoError) -> Self {
        CustomError::IoError(error)
    }
}


fn get_local_tags(imgpath: &str) -> HashSet<String> {
    match Metadata::new_from_path(imgpath) {
        Ok(metadata) => metadata
            .get_tag_multiple_strings("Xmp.dc.subject")
            .expect("failed to get xmp tag")
            .into_iter()
            .collect::<HashSet<String>>(),
        Err(err) => {
            eprintln!("ERROR on {}, {}", imgpath, err);
            HashSet::<String>::new()
        }
    }
}

fn scan_folder(folder_path: &str, table: &mut Table) -> Result<(), Box<dyn Error>> {
    let mut add_to_table = |abs_path_buf: PathBuf| -> Result<(), Box<dyn Error>> {
        // unwrap or default: in case of files with no extension(like.Xrresouces)
        let extension_str = abs_path_buf
            .extension()
            .and_then(OsStr::to_str)
            .unwrap_or_default();
        match extension_str.to_lowercase().as_str() {
            "png" | "jpg" | "bmp" | "jpeg" | "tif" | "tiff" | "webp" => {
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
        };
        Ok(())
    };

    for result in Walk::new(folder_path) {
        if let Ok(entry) = result {
            add_to_table(entry.into_path());
        }
    }
    Ok(())
}

fn tag_single_image(
    abspath: &str,
    url: &str,
    min_similarity: f64,
    album_path: &str,
) -> Result<(i64, i64), io::Error> {
    let file_not_exist_err = io::Error::new(ErrorKind::NotFound, "File deleted or removed");
    let parse_err = io::Error::new(ErrorKind::InvalidData, "Failed to parse json");
    let network_err_saucenao = io::Error::new(ErrorKind::NotConnected, "Failed to get online tag from saucenao");
    let quota_exceed_err_saucenao = io::Error::new(ErrorKind::OutOfMemory, "Quota exceed for saucenao");
    let network_err_gelbooru = io::Error::new(ErrorKind::NotConnected, "Failed to get online tag from gelbooru");
    
    let rel_path_str = Path::new(abspath)
        .strip_prefix(album_path)
        .unwrap_or(Path::new(""))
        .to_str()
        .unwrap_or_default();
    println!("Image: {}", rel_path_str);
    // check whether the path exists, if not, return an error to remove it from table
    if !Path::new(abspath).exists() {
        return Err(file_not_exist_err);
    }

    // send request to saucenao
    let form = reqwest::blocking::multipart::Form::new().file("file", abspath)?;
    let resp = Client::new().post(url).multipart(form).send().or(Err(network_err_saucenao))?;
    // validate the response
    if resp.status().is_server_error() {
        eprintln!("server error!");
        return Err(network_err_saucenao);
    } else if !resp.status().is_success() {
        match resp.status() {
            reqwest::StatusCode::TOO_MANY_REQUESTS => {
                println!("No quota left. Waiting for next scan...");
                return Err(quota_exceed_err_saucenao);
            }
            _ => println!("Something else happened. Status: {:?}", resp.status()),
        };
        return Err(network_err_saucenao);
    }

    // parsing result from saucenao

    let resp_json = resp.json::<serde_json::Value>().or(Err(parse_err))?;
    let short_limit: i64 = resp_json["header"]["short_limit"]
        .as_str()
        .ok_or(parse_err)?
        .parse()
        .or(Err(parse_err))?;
    let short_remain: i64 = resp_json["header"]["short_remaining"]
        .as_i64()
        .ok_or(parse_err)?;
    let long_limit: i64 = resp_json["header"]["long_limit"]
        .as_str()
        .ok_or(parse_err)?
        .parse()
        .or(Err(parse_err))?;
    let long_remain: i64 = resp_json["header"]["long_remaining"]
        .as_i64()
        .ok_or(parse_err)?;
    let similarity: f64 = resp_json["results"][0]["header"]["similarity"]
        .as_str()
        .ok_or(parse_err)?
        .parse()
        .or(Err(parse_err))?;
    // filter similarity
    if similarity <= min_similarity {
        println!("[Short limit: {}/{}]  Similarity for {} is too low, ignore.", short_remain, short_limit, rel_path_str);
    } else {
        // parse gelbooru id
        let gelbooru_id: i64 = resp_json["results"][0]["data"]["gelbooru_id"]
            .as_i64()
            .ok_or(parse_err)?;
        // get tags from gelbooru
        let json_url = format!(
            "https://gelbooru.com/index.php?page=dapi&s=post&q=index&json=1&id={}",
            gelbooru_id
        );
        let online_tags =
            match &reqwest::blocking::get(&json_url).or(Err(network_err_gelbooru))?.json::<serde_json::Value>().or(Err(parse_err))?["post"][0]["tags"] {
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
                .set_tag_multiple_strings("Xmp.dc.subject", &new_tags)
                .expect("Unable to get tags");
            match metadata.save_to_file(abspath) {
                Err(_) => println!("Failed to save tags for {}", abspath),
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

fn tag_all_images(
    table: &mut Table,
    url: &str,
    min_similarity: f64,
    table_path: &str,
    preserve_quota_percent: f64,
    rescan_interval_minutes: u64,
    cache_num: u64,
    album_path: &str,
) {
    let mut entry_to_add_back = Vec::new();
    let mut is_startup = true;
    let mut long_quota: i64 = (&table).len() as i64;
    while long_quota > 0 && !table.is_empty() {
        match table.pop() {
            Some((rel_path, cnt)) => {
                let abspath = format!("{}{}", album_path, &rel_path);
                match tag_single_image(&abspath, url, min_similarity, album_path) {
                    Ok((long_remain, long_limit)) => {
                        if long_remain > 0 && long_limit > 0 {
                            // update available_quota
                            long_quota = long_remain as i64
                                - (long_limit as f64 * preserve_quota_percent / 100.0).ceil() as i64;
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
                                println!("File {} deleted or removed.", abspath);
                                // file is deleted, we won't add it back into the table
                                continue;
                            }
                            ErrorKind::NotConnected => {
                                println!("Failed to get online tags from gelbooru");
                                long_quota = 0;
                            }
                            ErrorKind::InvalidData => {
                                println!("Failed to parse json from sacenao");
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
                // so if an image has been tagged for 4 times, it will reset to 1.
                let cnt_new = if cnt < 4 { cnt + 1 } else { 1 };
                entry_to_add_back.push((&rel_path, cnt_new));
            }
            None => {
                eprintln!("Table inconsistancy");
                break;
            }
        }
        // write table into disk if idx % cache_num == 0
        if entry_to_add_back.len() as u64 % cache_num == 0 {
            save_table(&table, table_path).expect("unable to save table");
        }
    }
    save_table(&table, table_path).expect("unable to save table");
    // finished one complete scan. wait for next folder scan
    thread::sleep(time::Duration::from_secs(60 * rescan_interval_minutes)); // long request limit
    scan_folder(&album_path, table).expect("unable to rescan the folder");
}

fn save_table(table: &Table, path: &str) -> io::Result<()> {
    let tmp_path = String::from(path) + "_tmp";
    let table_file = File::create(&tmp_path)?;
    serde_json::to_writer(table_file, &table.hashtable)
        .expect("Failed to serialize table before saving!");
    fs::rename(&tmp_path, &path)?;
    let covered = table.hashtable.iter().filter(|(_, &x)| x != 0).count();
    println!(
        "Table Saved!  Images covered: {} / {} ",
        covered,
        table.pq.len()
    );
    Ok(())
}

fn read_table(path: &str) -> Table {
    fn read_hashtable(path: &str) -> Result<HashMap<String, u8>, Box<dyn Error>> {
        let table_file = File::open(path)?;
        let hashtable = serde_json::from_reader(table_file)?;
        Ok(hashtable)
    }

    let hashtable = match read_hashtable(path) {
        Err(e) => {
            println!("No existing table, create a new table");
            println!("{}", e);
            HashMap::new()
        }
        Ok(hashtable) => {
            println!("Table loaded! Totally {} images!", hashtable.len());
            hashtable
        }
    };

    let vec = hashtable
        .iter()
        .map(|(s, u)| (u.clone(), s.as_str()))
        .collect::<Vec<(u8, &str)>>();

    let pq = BinaryHeap::from_iter(hashtable.iter().map(|(s, u)| (u.clone(), s.as_str())));

    Table::new(hashtable)
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
    let config = Config::builder()
        .set_default("table_path", "./table.json")?
        .set_default("album_path", "./")?
        .set_default("api_key", "")?
        .set_default("min_similarity", 55)?
        .set_default("preserve_quota_percent", 25)?
        .set_default("rescan_interval_minutes", 5)?
        .set_default("cache_num", 3)?
        .add_source(config::File::new(config_path, config::FileFormat::Json))
        .build()?;
    let album_path = config
        .get_string("album_path")
        .expect("album_path must be a string!");
    let table_path = config
        .get_string("table_path")
        .expect("table_path must be a string!");
    let api_key = config
        .get_string("api_key")
        .expect("api_key must be a string!");
    let min_similarity = config
        .get_float("min_similarity")
        .expect("min_similarity must be a f64 float!");
    let preserve_quota_percent = config
        .get_float("preserve_quota_percent")
        .expect("preserve_quota_percent must be a f64 float!");
    let rescan_interval_minutes: u64 = config
        .get_int("rescan_interval_minutes")?
        .try_into()
        .expect("cache_num must be an u64 integer!");
    let cache_num: u64 = config
        .get_int("cache_num")?
        .try_into()
        .expect("cache_num must be an u64 integer!");
    let url = format!(
        "https://saucenao.com/search.php?output_type=2&dbmask=16777216&numres=1&api_key={}",
        api_key
    );

    let mut table = read_table(&table_path);
    scan_folder(&album_path, &mut table)?;
    save_table(&table, &table_path).expect("Unable to save the table");
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
}
