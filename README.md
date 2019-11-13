# waifu_image_tagger

Another saucenao image tagger

It scans your album folder (recursively), find images(jpg, png) and get tags from gelbooru, finally write them into the IPTC-IIM keywords of each image.

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
