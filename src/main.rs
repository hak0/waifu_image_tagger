extern crate clap;
extern crate reqwest;
extern crate rexiv2;
extern crate rustnao;
extern crate serde_json;
use clap::App;
use rexiv2::Metadata;
use rustnao::{Handler, HandlerBuilder};
use std::collections::{BTreeSet, BTreeMap};
use std::error::Error;
use std::fs::{self, File};
use std::io::{self, BufReader};
use std::path::{Path, PathBuf};
use std::result::Result;
use std::sync::{Arc, Mutex};
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

fn scan_folder(
    folder_path: &str,
    table: Arc<Mutex<BTreeMap<String, u8>>>,
) -> Result<(), Box<dyn Error>> {
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
                let mut table = table.lock().unwrap();
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
                    if path.file_name() != Some(std::ffi::OsStr::new("@eaDir")) { // For synology systems
                        visit_dirs(&path, cb)?;
                    }
                } else {
                    cb(&path);
                }
            }
        }
        Ok(())
    };

    let path = Path::new(folder_path);
    visit_dirs(path, &mut add_to_table).expect("Failed to add images into hashmap");
    Ok(())
}

fn tag_single_image(
    abspath: &str,
    table: Arc<Mutex<BTreeMap<String, u8>>>,
    handle: Arc<Mutex<Handler>>,
    album_path: String,
) -> Result<(), Box<dyn Error>> {
    use rustnao::ErrType;

    let handle = handle.lock().unwrap();
    while match handle.get_sauce(abspath, None, None) {
        Err(err) => match err.kind() {
            ErrType::InvalidFile(_) => {
                println!("File {} deleted or removed.", abspath);
                let mut table = table.lock().unwrap();
                let rel_path_str = Path::new(abspath)
                    .strip_prefix(album_path.clone())
                    .unwrap_or(Path::new(""))
                    .to_str()
                    .unwrap_or_default();
                (*table).remove(rel_path_str);
                false
            }
            ErrType::InvalidRequest(_) => {
                println!("Invalid Request for {}", abspath);
                let mut table = table.lock().unwrap();
                let rel_path_str = Path::new(abspath)
                    .strip_prefix(album_path.clone())
                    .unwrap_or(Path::new(""))
                    .to_str()
                    .unwrap_or_default();
                match table.get_mut(rel_path_str) {
                    Some(val) => {
                        if *val != std::u8::MAX {
                            *val += 1;
                        } else {
                            *val = 1;
                        }
                    }
                    _ => {
                        table.insert(rel_path_str.to_owned(), 0);
                    }
                };
                false
            }
            ErrType::InvalidCode { code, message } => match code {
                -5 => {
                    println!("Image file size > 15MB, too large!");
                    false
                }
                -4 => {
                    println!("Image not valid {}", abspath);
                    false
                }
                -2 => {
                    println!("Limit Exceeded, Wait 30 minutes.",);
                    thread::sleep(time::Duration::from_secs(1800));
                    true
                }
                _ => {
                    println!("Err: {}", message);
                    println!("ErrKind: {:?}", err.kind());
                    println!("ErrCode: {}", code);
                    println!("Filepath: {}", abspath);
                    true
                }
            },
            _ => {
                println!("Err: {:?}", err);
                println!("ErrKind: {:?}", err.kind());
                println!("Filepath: {}", abspath);
                thread::sleep(time::Duration::from_secs(30));
                true
            }
        },
        Ok(result) => {
            let online_tags = match result.first() {
                None => BTreeSet::<String>::new(),
                Some(result_first) => {
                    let gelbooru_id = result_first
                        .additional_fields
                        .as_ref()
                        .ok_or("Failed to get additional fields of response")
                        .unwrap()["gelbooru_id"]
                        .as_u64()
                        .ok_or("failed to convert gelbooru_id into u64")
                        .unwrap();
                    let json_url = format!(
                        "https://gelbooru.com/index.php?page=dapi&s=post&q=index&json=1&id={}",
                        gelbooru_id
                    );
                    match serde_json::from_str::<serde_json::Value>(&reqwest::get(&json_url)
                        .expect("failed to get response from gelbooru")
                        .text()?)
                    {
                        Ok(v) => v[0]["tags"]
                            .to_string()
                            .replace("\"", "")
                            .split(" ")
                            .map(|s| s.to_owned())
                            .collect::<BTreeSet<String>>(),
                        Err(_) => {
                            println!("failed to deserialize json");
                            BTreeSet::<String>::new()
                        }
                    }
                }
            };
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
            let mut table = table.lock().unwrap();
            let rel_path_str = Path::new(abspath)
                .strip_prefix(album_path.clone())
                .unwrap_or(Path::new(""))
                .to_str()
                .unwrap_or_default();
            match table.get_mut(rel_path_str) {
                Some(val) => {
                    if *val != std::u8::MAX {
                        *val += 1;
                    } else {
                        *val = 1;
                    }
                }
                _ => {
                    table.insert(rel_path_str.to_owned(), 0);
                }
            }
            println!(
                "[Short limit: {}/{}]  Updated {}",
                handle.get_current_short_limit(),
                handle.get_short_limit(),
                rel_path_str
            );
            false
        }
    } {
        ();
    }
    Ok(())
}

fn tag_all_images(
    table_lock: Arc<Mutex<BTreeMap<String, u8>>>,
    handle_lock: Arc<Mutex<Handler>>,
    table_path: &str,
    preserve_quota_percent: f64,
    rescan_interval_minutes: u64,
    cache_num: u64,
    album_path: String,
) {
    use std::convert::TryFrom;
    loop {
        // in order to get the correct limit, we have to tag an image at first.
        {
            let rel_path = table_lock
                .lock()
                .unwrap()
                .iter()
                .min_by_key(|x| x.1)
                .unwrap()
                .0
                .to_owned();
            let abspath = format!("{}{}", album_path, rel_path);
            tag_single_image(
                &abspath,
                table_lock.clone(),
                handle_lock.clone(),
                album_path.clone(),
            )
            .expect(&format!("Failed to tag single image {}", &abspath));
        };
        let long_limit = handle_lock.lock().unwrap().get_long_limit();
        let current_long_limit = handle_lock.lock().unwrap().get_current_long_limit();
        println!("Current Long Limit: {}", current_long_limit); //表示每日可用的总张数
        println!("Long Limit: {}", long_limit); //表示剩余可用的张数
        let available_quota = current_long_limit as i64
            - (long_limit as f64 * preserve_quota_percent / 100.0).ceil() as i64;
        if available_quota > 0 {
            let mut available_quota: usize = usize::try_from(available_quota).unwrap();
            let table_len = table_lock.lock().unwrap().len();
            if available_quota > table_len {
                available_quota = table_len;
            };
            println!("Going to tag {} images...", available_quota);
            let ptable = table_lock.lock().unwrap();
            let mut vec = (&ptable)
                .iter()
                .map(|(s, u)| (s.to_owned(), u.to_owned()))
                .collect::<Vec<(String, u8)>>();
            std::mem::drop(ptable);
            vec.sort_by(|a, b| a.1.partial_cmp(&(b.1)).unwrap());
            let selected = vec[0..available_quota].to_vec();
            let mut count: u64 = cache_num;
            for (relpath, _) in selected.iter() {
                let abspath = format!("{}{}", album_path, relpath);
                tag_single_image(
                    &abspath,
                    table_lock.clone(),
                    handle_lock.clone(),
                    album_path.clone(),
                )
                .expect(&format!("Failed to tag image {}", abspath));
                count = count - 1;
                if count <= 0 {
                    save_table(table_lock.clone(), table_path).expect("unable to save table");
                    count = cache_num;
                };
                {
                    let handle = handle_lock.lock().unwrap();
                    let current_short_limit = handle.get_current_short_limit();
                    thread::sleep(time::Duration::from_micros(
                        (30 * 1000 * 1000 / (current_short_limit)) as u64,
                    )); // short request limit
                }
            }
        }
        save_table(table_lock.clone(), table_path).expect("unable to save table");
        thread::sleep(time::Duration::from_secs(60 * rescan_interval_minutes)); // long request limit
        scan_folder(&album_path, table_lock.clone()).expect("uanble to rescan the folder");
    }
}

fn save_table(table: Arc<Mutex<BTreeMap<String, u8>>>, path: &str) -> io::Result<()> {
    let table_file = File::create(path)?;
    let p_table = table.lock().unwrap();
    serde_json::to_writer(table_file, &(*p_table))
        .expect("Failed to serialize table before saving!");
    let covered = (*p_table).iter().filter(|(_, &x)| x != 0).count();
    println!(
        "Table Saved!  Images covered: {} / {} ",
        covered,
        (*p_table).len()
    );
    Ok(())
}

fn read_table(table: Arc<Mutex<BTreeMap<String, u8>>>, path: &str) -> io::Result<()> {
    match File::open(path) {
        Ok(table_file) => {
            let mut table = table.lock().unwrap();
            let table2: BTreeMap<String, u8> = serde_json::from_reader(table_file)?;
            (*table).extend(table2);
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
    let config_path = match App::new("waifu image tagger")
        .args_from_usage("-c, --config=[FILE] 'set a config file'")
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
    let table_lock = Arc::new(Mutex::new(BTreeMap::<String, u8>::new()));
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
    let handle_lock = Arc::new(Mutex::new(
        HandlerBuilder::new()
            .api_key(api_key)
            .min_similarity(min_similarity)
            .db(Handler::GELBOORU)
            .build(),
    ));
    read_table(table_lock.clone(), table_path).expect("Failed to read table!");
    scan_folder(album_path, table_lock.clone())?;
    if table_lock.lock().unwrap().is_empty() {
        save_table(table_lock.clone(), table_path).expect("Unable to save the table");
    }
    let c_table_lock = table_lock.clone();
    let c_handle_lock = handle_lock.clone();
    let c_table_path = table_path.to_owned();
    let c_album_path = album_path.to_owned();
    tag_all_images(
        c_table_lock,
        c_handle_lock,
        &c_table_path,
        preserve_quota_percent,
        rescan_interval_minutes,
        cache_num,
        c_album_path,
    );
    Ok(())
}
