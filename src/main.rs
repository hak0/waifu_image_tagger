extern crate rustnao;
extern crate notify;
use rustnao::{Handler, HandlerBuilder};
use std::collections::HashMap;
use std::fs::File;
use std::io;

fn watch_folder(folder_path: &str, table: &mut HashMap<String, u8>) -> notify::Result<()> {
    use crossbeam_channel::unbounded;
    use notify::{RecommendedWatcher, RecursiveMode, Result, Watcher, EventKind};
    use std::time::Duration;

    let (tx, rx) = unbounded();
    let mut watcher: RecommendedWatcher = Watcher::new(tx, Duration::from_secs(5))?;
    watcher.watch(folder_path, RecursiveMode::Recursive)?;
    loop {
        match rx.recv() {
            Ok(event) => {
                let event_unwrap = event.unwrap();
                match (&event_unwrap.kind, &event_unwrap.flag()) {
                    (EventKind::Create(_), None) |
                    (EventKind::Remove(_), None) => {
                        scan_folder(folder_path, table);
                        println!("Change Detected: {:?}, {:?}, Table Updated", event_unwrap.kind, event_unwrap.paths);
                    },
                    _ => (),
                }
            },
            Err(err) => println!("watch error: {:?}", err),
        };
    }
    Ok(())
}

fn scan_folder(folder_path: &str, table: &mut HashMap<String, u8>) {
    use std::fs;
    use std::path::{Path, PathBuf};

    let mut add_to_table = |path: &PathBuf| {
        let filepath = path.to_str().unwrap().to_owned();
        let extension = match path.extension() {
            None => "",
            Some(os_str) => match os_str.to_str() {
                None => "",
                Some(str) => str,
            },
        };
        //TODO: 检查是否有标签，若有则以1插入
        //TODO: 在索引的时候，如果图片不存在，则直接从hashmap中删除本条
        match extension {
            "png" | "jpg" | "bmp" | "gif" | "jpeg" | "tif" | "tiff" | "webp" => {
                table.entry(filepath).or_insert(0);
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
}

fn test_rustnao() {
    let handle = HandlerBuilder::new()
        .api_key("your_api_key")
        .num_results(999)
        //TODO: change similarity in config file
        .min_similarity(50.0)
        .db(Handler::DANBOORU)
        .build();
    let result = handle.get_sauce("./tests/test2.jpg", None, None);
    println!("{:?}", result);
    println!("Result is empty?: {}", result.unwrap().is_empty()); //表示每日可用的总张数
    println!("Current Long Limit{}", handle.get_current_long_limit()); //表示剩余可用的张数
    println!("Long Limit{}", handle.get_long_limit());
    println!("Current Short Limit{}", handle.get_current_short_limit());
    println!("Short Limit{}", handle.get_short_limit());
}

fn save_table(table: &mut HashMap<String, u8>, path: &str) -> io::Result<()> {
    let table_file = File::create(path)?;
    let datas: Vec<(_, _)> = table.into_iter().collect();
    bincode::serialize_into(table_file, &datas).expect("Failed to serialize table before saving!");
    Ok(())
}

fn read_table(table: &mut HashMap<String, u8>, path: &str) -> io::Result<()> {
    let table_file = File::open(path)?;
    let decoded: Vec<(String, u8)> = bincode::deserialize_from(table_file).unwrap();
    for (key, value) in decoded {
        table.insert(key.to_owned(), value.to_owned());
    }
    Ok(())
}

fn main() {
    let mut table: HashMap<String, u8> = HashMap::new();
    if true {
        watch_folder("./tests", &mut table);
        // scan_folder("./tests", &mut table);
        // scan_folder("/mnt/f/SynologyDrive/Moments/main", &mut table);
        // println!("Serde Size: {:?} mb", (bincode::serialize(&table).unwrap().len()*8) as f64 /1024.0/1024.0);
        // save_table(&mut table, "./tests/table").expect("Failed to save table!");
    } else {
        read_table(&mut table, "./tests/table").expect("Failed to read table!");
        for (key, value) in table {
            println!("{}: {}", key, value);
        }
    }
    // test_rustnao();
    println!("Hello, world!");
}
