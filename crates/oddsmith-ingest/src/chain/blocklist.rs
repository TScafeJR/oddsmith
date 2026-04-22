//! Known Polymarket protocol and infrastructure addresses on Polygon.

/// Polygon contract addresses whose direct incoming/outgoing transfers are treated as
/// on-platform settlement, not P2P wallet links. Extend this list as needed (see the
/// Polymarket docs / Polygonscan).
pub const POLYMARKET_PROTOCOL_ADDRESSES: &[&str] = &[
    // Core protocol contracts
    "0x4d97dcd97ec945f40cf65f87097ace5ea0476045",  // ConditionalTokens (CTF)
    "0x4bfb41d5b3570defd03c39a9a4d8de6bd8b8982e",  // CTF exchange (legacy deployment)
    "0xc5d563a36ae78145c45a50134d48a1215220f80a",  // Neg-risk CTF exchange (common)
    "0xd91e80cf2e7be2e162c6513cec4ca3393db93118",  // Polymarket proxy factory (relayer infra)
    "0xaacfaea03eb1562faf0044bebc8b342cfcf0385c5", // Proxy wallet factory (variant)
    // Infrastructure hubs: 200+ on-chain connections, no/minimal trading activity
    "0xf70da97812cb96acdf810712aa562db8dfa3dbef", // relay/settlement (397 connections)
    "0x3a3bd7bb9528e159577f7c2e685cc81a765002e2", // relay/settlement (355 connections)
    "0xc536633ff12ee52e280b2af2594031060c5aaf41", // relay/settlement (269 connections)
    "0xe672e75b6e824cda8e66c4b0bfce0668803cfac0", // relay/settlement (239 connections)
    "0x64131b60502c01362639cc5cfe1a1efb6be1165a", // relay/settlement (238 connections)
    "0xc288480574783bd7615170660d71753378159c47", // relay/settlement (236 connections)
    "0xf105cd06bf6ff2e8c9233a64ebf79ab85034709b", // relay/settlement (235 connections)
    "0x8129b737912e17212c8693b781928f5d0303390a", // relay/settlement (233 connections)
    "0x4da9395388791c22684e03779c3de10934eb9cfb", // relay/settlement (232 connections)
    "0x4fac062e3edf91d976ea75ea6007cc3b086daad3", // relay/settlement (231 connections)
    "0xa7be1729f709955d7e0081cf07806e9f806dae26", // relay/settlement (230 connections)
    "0x710be84efbd0dfd4b1a67b9525a778ae5c420ff8", // relay/settlement (226 connections)
    "0x7bc41f2e80aaf83f971ed557f0c277e4aa9054a7", // relay/settlement (226 connections)
    "0xf7cd89be08af4d4d6b1522852ced49fc10169f64", // relay/settlement (221 connections)
    "0x09583cb666c9a687adeafb50fd556adb6bae6fa6", // relay/settlement (220 connections)
    "0x02a86f51aa7b8b1c17c30364748d5ae4a0727e23", // relay/settlement (219 connections)
    "0x7db63fe6d62eb73fb01f8009416f4c2bb4fbda6a", // relay/settlement (218 connections)
    "0x6efdade3d235fdebdf9c4fb61424809fddde48fc", // relay/settlement (217 connections)
    "0x9f75231fbbc2262036a0fd029c06235d897b8e7b", // relay/settlement (216 connections)
    "0xddafac1c6aaab70d25dcc31a6947f6095c6a5b3d", // relay/settlement (211 connections)
    "0xddd8c93165245b2764dfe8be340992e99d7b2c07", // relay/settlement (207 connections)
    "0x239b9c1f24f3423062b0d364796e07ee905e9fce", // relay/settlement (202 connections)
    "0x0c0e270cf879583d6a0142fc817e05b768d0434e", // infra (370 conn, 12 trades)
    "0x7c3db723f1d4d8cb9c550095203b686cb11e5c6b", // infra (153 conn, 23 trades)
];

/// Lowercase 0x-prefixed hex, or empty if it doesn't look like an address.
pub fn normalize_address(a: &str) -> String {
    let a = a.trim().to_lowercase();
    if !a.starts_with("0x") || a.len() < 42 {
        return String::new();
    }
    a
}

/// Whether `addr` is a known Polymarket settlement contract (or not a usable address).
pub fn is_polymarket_protocol(addr: &str) -> bool {
    let n = normalize_address(addr);
    n.is_empty() || POLYMARKET_PROTOCOL_ADDRESSES.contains(&n.as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protocol_addresses_and_junk_are_filtered() {
        assert!(is_polymarket_protocol(" 0x4D97DCD97EC945F40CF65F87097ACE5EA0476045 "));
        assert!(is_polymarket_protocol("not an address"));
        assert!(!is_polymarket_protocol("0x1111111111111111111111111111111111111111"));
    }
}
