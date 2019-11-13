# waifu_image_tagger

Another saucenao image tagger

It watches your album folder (recursively), find images(jpg, png) and get tags from gelbooru, finally write them into the IPTC-IIM keywords of each image.

Then you can use software like Adobe Lightroom or XMView to search images with tags. It will be quite useful when you are drowning in thousands of waifu images.

## Dependencies

gexiv2: see https://github.com/felixc/rexiv2/blob/master/SETUP.md

## Usage

```
cargo run --release
```

Read `config.json` by default.  
If the config doesn't exist, the current folder will be regarded as album path.

You can also use your custom config.json:

```
cargo build --release
./target/release/waifu_image_tagger -c your_config.json
```
## Config

When the program is running, it will detect creation of new images and tag them at once. In addition, it will rescan the album and update the tags of old images if you have extra saucenao quota.

The status of tagging will be saved into a hashmap, and it will be synced to disk in a regular basis in case of unexpected poweroff. 

`table_path`: the path to save the tag status of each image.  
`album_path`: the path of your album.  
`api_key`: saucenao api key. If you don't have one, just leave it empty. An upgraded saucenao account can tag more images a day.  
`min_similarity`: similarity threshold for saucenao. When it's too low, you may get wrong tags, but if it's too high, then you may miss the correct results. The value is between 0 ~ 100.  
`preserve_quota_percent`: it will preserve some percent of quota so you can still use saucenao in browser. The value is between 0 ~ 100. Set it to 0 to disable the feature.   
`rescan_interval_minutes`: after each rescan of all images, it will sleep for a while. The duration is set here.  
`cache_num`: the table will be written to disk each time after cache_num images are tagged.
