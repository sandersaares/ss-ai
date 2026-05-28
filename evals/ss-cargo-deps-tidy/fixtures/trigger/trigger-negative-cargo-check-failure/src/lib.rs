pub struct Header {
    pub version: u8,
}

pub fn parse(input: &[u8]) -> Header {
    let h = parse_header(input);
    h
}
