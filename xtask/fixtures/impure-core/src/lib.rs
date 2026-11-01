//! Each function uses something the pure crates must not use.

pub fn wall_clock() -> std::time::SystemTime {
    std::time::SystemTime::now()
}

pub fn monotonic() -> std::time::Instant {
    std::time::Instant::now()
}

pub fn read_file() -> std::io::Result<Vec<u8>> {
    std::fs::read("input")
}

pub fn open_file() -> std::io::Result<std::fs::File> {
    std::fs::File::open("input")
}

pub fn connect() -> std::io::Result<std::net::TcpStream> {
    std::net::TcpStream::connect("127.0.0.1:9")
}
