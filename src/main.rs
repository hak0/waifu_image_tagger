extern crate notify;
extern crate reqwest;
extern crate rexiv2;
extern crate rustnao;
extern crate serde_json;
use rexiv2::Metadata;
use rustnao::{Handler, HandlerBuilder};
use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::io;
use std::result::Result;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time;

const TABLE_PATH: &str = "";
const WATCH_FOLDER: &str = ""; // ends with "/"
const API_KEY: &str = "";
const MIN_SIMILARITY: f32 = 50.0;
const PRESERVED_QUOTA_PERCENT: f32 = 25.0;
const UPDATE_INTERVAL: u64 = 120;
const CACHE_NUM: u64 = 3;

fn watch_folder(folder_path: &str, table: Arc<Mutex<HashMap<String, u8>>>) -> notify::Result<()> {
    use crossbeam_channel::unbounded;
    use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
    use std::time::Duration;

    let (tx, rx) = unbounded();
    let mut watcher: RecommendedWatcher = Watcher::new(tx, Duration::from_secs(4))?;
    watcher.watch(folder_path, RecursiveMode::Recursive)?;
    loop {
        match rx.recv() {
            Ok(event) => {
                let event_unwrap = event.unwrap();
                match (&event_unwrap.kind, &event_unwrap.flag()) {
                    (EventKind::Create(_), None) => {
                        scan_folder(folder_path, table.clone())
                            .expect("Unable to scan the folder!");
                        println!("File Added: {:?}, Table Updated", event_unwrap.paths);
                        save_table(table.clone(), TABLE_PATH)?;
                    }
                    _ => (),
                }
            }
            Err(err) => eprintln!("filesystem watch error: {:?}", err),
        };
    }
}

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
    table: Arc<Mutex<HashMap<String, u8>>>,
) -> Result<(), Box<dyn std::error::Error>> {
    use std::fs;
    use std::path::{Path, PathBuf};

    let mut add_to_table = |path: &PathBuf| {
        let filepath = path.to_str().expect("failed to convert path into str");
        let extension = path
            .extension()
            .unwrap_or_default()
            .to_str()
            .unwrap_or_default();
        //TODO: 在索引的时候，如果图片不存在，则直接从hashmap中删除本条
        match extension {
            "png" | "jpg" | "bmp" | "gif" | "jpeg" | "tif" | "tiff" | "webp" => {
                let rel_path_str = path
                    .strip_prefix(folder_path)
                    .unwrap_or(Path::new(""))
                    .to_str()
                    .unwrap_or_default();
                let mut table = table.lock().unwrap();
                if !table.contains_key(rel_path_str) {
                    table.insert(
                        rel_path_str.to_owned(),
                        match get_local_tags(filepath).is_empty() {
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
                    visit_dirs(&path, cb)?;
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

fn tag_all_images(table: Arc<Mutex<HashMap<String, u8>>>) {
    use std::path::{Path, PathBuf};
    use std::convert::TryFrom;

    fn tag_single_image(abspath: &str, table: Arc<Mutex<HashMap<String, u8>>>) {
        let handle = HandlerBuilder::new()
            .api_key(API_KEY)
            .min_similarity(MIN_SIMILARITY)
            .db(Handler::GELBOORU)
            .build();
        while match handle.get_sauce(abspath, None, None) {
            Err(err) => {
                println!("Err: {}", err);
                println!("Filepath: {}", abspath);
                thread::sleep(time::Duration::from_secs(30));
                true
            },
            Ok(result) => {
                let online_tags =  if result.is_empty() {
                    BTreeSet::<String>::new()
                } else {
                    let gelbooru_id = result
                        .first()
                        .unwrap()
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
                    match reqwest::get(&json_url)
                        .expect("failed to get response from gelbooru")
                        .text()
                    {
                        Ok(json) => {
                            let v: serde_json::Value = serde_json::from_str(&json).unwrap();
                            v[0]["tags"]
                                .to_string()
                                .replace("\"", "")
                                .split(" ")
                                .map(|s| s.to_owned())
                                .collect::<BTreeSet<String>>()
                        }
                        Err(err) => {
                            println!("Failed to get json from gelbooru");
                            BTreeSet::<String>::new()
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
                    let metadata = Metadata::new_from_path(abspath).unwrap();
                    metadata.set_tag_multiple_strings("Iptc.Application2.Keywords", &new_tags);
                    metadata.save_to_file(abspath);
                }
                let mut table = table.lock().unwrap();
                let rel_path_str = Path::new(abspath)
                    .strip_prefix(WATCH_FOLDER)
                    .unwrap_or(Path::new(""))
                    .to_str()
                    .unwrap_or_default();
                // println!("{:?}", (*p_table).entry(rel_path_str.to_string()).or_insert(0));
                match table.get_mut(rel_path_str) {
                    Some(val) => {
                    if *val != std::u8::MAX {
                        *val += 1;
                    } else {
                        *val = 1;
                    }},
                    _ => {table.insert(rel_path_str.to_owned(), 0);},
                }
                println!("Current Short Limit {}  Total {}", handle.get_current_short_limit(), handle.get_short_limit());
                false
            },
        } {
            ();
        }
    };

    // let p_table = table.lock().unwrap();
    let handle = HandlerBuilder::new()
                .api_key(API_KEY)
                .min_similarity(MIN_SIMILARITY)
                .db(Handler::GELBOORU)
                .build();
    println!("Current Long Limit{}", handle.get_current_long_limit()); //表示每日可用的总张数
    println!("Long Limit{}", handle.get_long_limit()); //表示剩余可用的张数
    let available_quota = handle.get_current_long_limit() as i32 - (handle.get_long_limit() as f32 * PRESERVED_QUOTA_PERCENT / 100.0).ceil() as i32;
    if available_quota > 0 {
        let mut available_quota: usize = usize::try_from(available_quota).unwrap();
        let table_len = table.lock().unwrap().len();
        if available_quota > table_len {
            available_quota = table_len;
        };
        let ptable = table.lock().unwrap();
        let mut vec = (&ptable).iter().map(|(s,u)| (s.to_owned(),u.to_owned())).collect::<Vec<(String, u8)>>();
        std::mem::drop(ptable);
        vec.sort_by(|a, b| a.1.partial_cmp(&(b.1)).unwrap());
        let selected = vec[0..available_quota].to_vec();
        let mut count:u64 = CACHE_NUM;
        for (relpath, _) in selected.iter() {
            let abspath = format!("{}{}", WATCH_FOLDER, relpath);
            tag_single_image(&abspath, table.clone());
            count = count - 1;
            if count <= 0{
                save_table(table.clone(), TABLE_PATH);
                count = CACHE_NUM;
            };
            let current_short_limit = handle.get_current_short_limit();
            let short_limit = handle.get_short_limit();
            thread::sleep(time::Duration::from_secs((30/(current_short_limit+1)+1) as u64)); // short request limit
        }
    }
    thread::sleep(time::Duration::from_secs(UPDATE_INTERVAL)); // long request limit
}

fn save_table(table: Arc<Mutex<HashMap<String, u8>>>, path: &str) -> io::Result<()> {
    let table_file = File::create(path)?;
    let p_table = table.lock().unwrap();
    serde_json::to_writer(table_file, &(*p_table))
        .expect("Failed to serialize table before saving!");
    println!("Table Saved! Total {} Images", (*p_table).len());
    Ok(())
}

fn read_table(table: Arc<Mutex<HashMap<String, u8>>>, path: &str) -> io::Result<()> {
    let table_file = File::open(path);
    match table_file {
        Ok(table_file) => {
            let mut table = table.lock().unwrap();
            let table2: HashMap<String, u8> = serde_json::from_reader(table_file).unwrap();
            (*table).extend(table2);
            println!("Table loaded! Totally {} images!", table.len());
        }
        Err(_) => println!("No existing table, create a new table"),
    }
    Ok(())
}

fn main() {
    let table = Arc::new(Mutex::new(HashMap::<String, u8>::new()));
    read_table(table.clone(), TABLE_PATH).expect("Failed to read table!");
    scan_folder(WATCH_FOLDER, table.clone()).expect("Unable to scan the folder");
    println!("table: {:?}", table.lock().unwrap());
    if table.lock().unwrap().is_empty() {
        save_table(table.clone(), TABLE_PATH).expect("Unable to save the table");
    }
    let table2 = table.clone();
    let tagworker = thread::spawn(move || -> () {
        tag_all_images(table2);
    });
    watch_folder(WATCH_FOLDER, table.clone()).expect("Failed to watch folder!");
    println!("Hello, world!");
}
