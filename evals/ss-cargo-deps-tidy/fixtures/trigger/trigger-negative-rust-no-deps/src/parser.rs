pub enum Packet {
    Hello,
    Ping,
    Goodbye,
}

pub fn parse_packet(byte: u8) -> Packet {
    match byte {
        0x01 => Packet::Hello,
        0x02 => Packet::Ping,
        0x03 => Packet::Goodbye,
        other => panic!("unknown packet variant: {other:#x}"),
    }
}
