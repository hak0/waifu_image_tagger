# waifu_image_tagger

Another saucenao image tagger

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
