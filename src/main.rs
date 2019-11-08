extern crate rustnao;
use rustnao::HandlerBuilder;

fn test_rustnao() {
    let handle = HandlerBuilder::new()
        .api_key("your_api_key")
        .num_results(999)
        .db(999)
        .build();
    let result = handle.get_sauce("./tests/test2.jpg", None, None);
    println!("{:?}", result);
}

fn main() {
    test_rustnao();
    println!("Hello, world!");
}
