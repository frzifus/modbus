mod server;

use server::{Server, SharedData};


fn main() {
    let mut srv = Server::new("0.0.0.0:1502".to_owned());
    srv.add_slave(1, SharedData::default()).unwrap();
    srv.run();
}
