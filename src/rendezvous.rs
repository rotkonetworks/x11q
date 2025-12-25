//! rendezvous - word-code based peer discovery over mainline dht
//!
//! uses pkarr to publish nodeid under a derived keypair, so both sides
//! can find each other using just a short word code like "7-tiger-lamp".
//! spake2 pake ensures only someone with the code can connect.

use anyhow::{Context, Result};
use ed25519_dalek::SigningKey;
use iroh::NodeId;
use pkarr::dns::{rdata::TXT, Name};
use pkarr::{Client as PkarrClient, Keypair, SignedPacket};
use rand::Rng;
use sha2::{Digest, Sha256};
use spake2::{Ed25519Group, Identity, Password, Spake2};
use std::time::Duration;
use tokio::time::timeout;

const DHT_TIMEOUT: Duration = Duration::from_secs(30);
const CODE_TTL: u32 = 120;

/// pgp-style wordlist (256 words, 8 bits each)
/// even words: 2 syllables, odd words: 3 syllables (helps error detection)
const WORDLIST: [&str; 256] = [
    // even (2 syllables)
    "aardvark",
    "absurd",
    "accrue",
    "acme",
    "adrift",
    "adult",
    "afflict",
    "ahead",
    "aimless",
    "algol",
    "allow",
    "almost",
    "ammo",
    "ancient",
    "apple",
    "artist",
    "assume",
    "atlas",
    "awesome",
    "axle",
    "baboon",
    "backfield",
    "backward",
    "banjo",
    "beaming",
    "bedlamp",
    "beehive",
    "beeswax",
    "befriend",
    "belfast",
    "berserk",
    "billiard",
    "bison",
    "blackjack",
    "blockade",
    "blowtorch",
    "bluebird",
    "bombast",
    "bookshelf",
    "brackish",
    "breadline",
    "breakup",
    "brickyard",
    "briefcase",
    "burbank",
    "button",
    "buzzard",
    "cement",
    "chairlift",
    "chatter",
    "checkup",
    "chessman",
    "chico",
    "chisel",
    "choking",
    "classic",
    "classroom",
    "cleanup",
    "clockwork",
    "cobra",
    "commence",
    "concert",
    "cowbell",
    "crackdown",
    "cranky",
    "crayon",
    "crossbow",
    "crowfoot",
    "crucial",
    "crusade",
    "cubic",
    "dashboard",
    "deadbolt",
    "deckhand",
    "decode",
    "detour",
    "digital",
    "diploma",
    "disrupt",
    "distant",
    "diver",
    "doorstep",
    "dosage",
    "dotted",
    "dragon",
    "dreadful",
    "drifter",
    "dropout",
    "drumbeat",
    "drunken",
    "duplex",
    "dwelling",
    "eating",
    "edict",
    "egghead",
    "eightball",
    "endorse",
    "endow",
    "enlist",
    "erase",
    "escape",
    "exceed",
    "eyeglass",
    "eyetooth",
    "facial",
    "fallout",
    "flagpole",
    "flatfoot",
    "flytrap",
    "fracture",
    "framework",
    "freedom",
    "frighten",
    "gazelle",
    "geiger",
    "glasgow",
    "glitter",
    "glucose",
    "goggles",
    "goldfish",
    "gremlin",
    "guidance",
    "hamlet",
    "hamster",
    "handiwork",
    "headwaters",
    "highchair",
    "hockey",
    // odd (3 syllables)
    "hamburger",
    "hesitate",
    "hideaway",
    "holiness",
    "hurricane",
    "hydraulic",
    "idaho",
    "implicit",
    "indulge",
    "inferno",
    "informant",
    "insincere",
    "insurgent",
    "intestine",
    "inventive",
    "japanese",
    "jupiter",
    "kickoff",
    "kingfish",
    "klaxon",
    "liberty",
    "maritime",
    "miracle",
    "misnomer",
    "molasses",
    "molecule",
    "montana",
    "mosquito",
    "multiple",
    "nagasaki",
    "narrative",
    "nebula",
    "newsletter",
    "nominal",
    "northward",
    "obscure",
    "october",
    "offload",
    "olive",
    "openwork",
    "operator",
    "optic",
    "orbit",
    "osmosis",
    "outfielder",
    "pacific",
    "pandemic",
    "pandora",
    "paperweight",
    "pedigree",
    "pegasus",
    "penetrate",
    "perceptive",
    "pharmacy",
    "phonetic",
    "photograph",
    "pioneering",
    "piracy",
    "playhouse",
    "populate",
    "potato",
    "preclude",
    "prescribe",
    "printer",
    "procedure",
    "puberty",
    "publisher",
    "pyramid",
    "quantity",
    "racketeer",
    "rampant",
    "reactor",
    "recipe",
    "recover",
    "renegade",
    "repellent",
    "replica",
    "reproduce",
    "resistor",
    "responsive",
    "retina",
    "retrieval",
    "revenue",
    "riverbed",
    "rosebud",
    "ruffian",
    "sailboat",
    "saturday",
    "savanna",
    "scavenger",
    "sensation",
    "sequence",
    "shadowbox",
    "showgirl",
    "signify",
    "simplify",
    "simulate",
    "slowdown",
    "snapshot",
    "snowcap",
    "snowslide",
    "solitude",
    "southward",
    "specimen",
    "speculate",
    "spellbound",
    "spheroid",
    "spigot",
    "spindle",
    "steadfast",
    "steamship",
    "stockman",
    "stopwatch",
    "stormy",
    "strawberry",
    "stupendous",
    "supportive",
    "surrender",
    "suspense",
    "sweatband",
    "swelter",
    "tampico",
    "telephone",
    "therapist",
    "tobacco",
    "tolerance",
    "tomorrow",
    "torpedo",
];

/// generate a random code: "N-word-word"
pub fn generate_code() -> String {
    let mut rng = rand::thread_rng();
    let n: u8 = rng.gen_range(0..100);
    let w1 = WORDLIST[rng.gen_range(0..256)];
    let w2 = WORDLIST[rng.gen_range(0..256)];
    format!("{}-{}-{}", n, w1, w2)
}

/// derive deterministic ed25519 keypair from code
/// used as pkarr identity for dht publish/lookup
fn derive_keypair(code: &str) -> Keypair {
    let mut hasher = Sha256::new();
    hasher.update(b"x11q-rendezvous-v1:");
    hasher.update(code.as_bytes());
    let seed: [u8; 32] = hasher.finalize().into();

    // pkarr keypair from seed
    let signing_key = SigningKey::from_bytes(&seed);
    Keypair::from_secret_key(&signing_key.to_bytes())
}

/// publish our nodeid to dht under the code's derived key
pub async fn publish_nodeid(code: &str, node_id: NodeId) -> Result<()> {
    let keypair = derive_keypair(code);
    let client = PkarrClient::builder().build()?;

    // encode nodeid as TXT record
    let node_id_hex = hex::encode(node_id.as_bytes());
    let name = Name::new("_x11q").context("invalid dns name")?;
    let txt = TXT::new()
        .with_string(&node_id_hex)
        .context("invalid txt")?;

    let packet = SignedPacket::builder()
        .txt(name, txt, CODE_TTL)
        .sign(&keypair)?;

    client.publish(&packet, None).await?;

    Ok(())
}

/// resolve nodeid from dht using code
pub async fn resolve_nodeid(code: &str) -> Result<NodeId> {
    let keypair = derive_keypair(code);
    let public_key = keypair.public_key();
    let client = PkarrClient::builder().build()?;

    let packet = timeout(DHT_TIMEOUT, client.resolve(&public_key))
        .await
        .context("dht lookup timed out")?
        .ok_or_else(|| anyhow::anyhow!("code not found on dht"))?;

    // find TXT record
    for record in packet.resource_records("_x11q") {
        if let pkarr::dns::rdata::RData::TXT(ref txt) = record.rdata {
            // convert TXT to String
            let txt_str: String = txt
                .clone()
                .try_into()
                .map_err(|_| anyhow::anyhow!("invalid utf8 in txt record"))?;
            let node_id_bytes = hex::decode(&txt_str).context("invalid nodeid encoding")?;
            let node_id = NodeId::from_bytes(
                &node_id_bytes
                    .try_into()
                    .map_err(|_| anyhow::anyhow!("wrong nodeid length"))?,
            )
            .context("invalid nodeid")?;
            return Ok(node_id);
        }
    }

    anyhow::bail!("no nodeid found in dht record")
}

/// spake2 side A (server)
pub struct PakeServer {
    spake: Spake2<Ed25519Group>,
    outbound_msg: Vec<u8>,
}

impl PakeServer {
    pub fn new(code: &str) -> Self {
        let (spake, outbound_msg) = Spake2::<Ed25519Group>::start_a(
            &Password::new(code.as_bytes()),
            &Identity::new(b"x11q-server"),
            &Identity::new(b"x11q-client"),
        );
        Self {
            spake,
            outbound_msg,
        }
    }

    pub fn message(&self) -> &[u8] {
        &self.outbound_msg
    }

    pub fn finish(self, client_msg: &[u8]) -> Result<[u8; 32]> {
        let key = self
            .spake
            .finish(client_msg)
            .map_err(|_| anyhow::anyhow!("pake failed - wrong code?"))?;
        Ok(key.try_into().expect("spake2 produces 32 byte key"))
    }
}

/// spake2 side B (client)
pub struct PakeClient {
    spake: Spake2<Ed25519Group>,
    outbound_msg: Vec<u8>,
}

impl PakeClient {
    pub fn new(code: &str) -> Self {
        let (spake, outbound_msg) = Spake2::<Ed25519Group>::start_b(
            &Password::new(code.as_bytes()),
            &Identity::new(b"x11q-server"),
            &Identity::new(b"x11q-client"),
        );
        Self {
            spake,
            outbound_msg,
        }
    }

    pub fn message(&self) -> &[u8] {
        &self.outbound_msg
    }

    pub fn finish(self, server_msg: &[u8]) -> Result<[u8; 32]> {
        let key = self
            .spake
            .finish(server_msg)
            .map_err(|_| anyhow::anyhow!("pake failed - wrong code?"))?;
        Ok(key.try_into().expect("spake2 produces 32 byte key"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_code_generation() {
        let code = generate_code();
        let parts: Vec<&str> = code.split('-').collect();
        assert_eq!(parts.len(), 3);
        assert!(parts[0].parse::<u8>().unwrap() < 100);
    }

    #[test]
    fn test_keypair_derivation_deterministic() {
        let k1 = derive_keypair("7-tiger-lamp");
        let k2 = derive_keypair("7-tiger-lamp");
        assert_eq!(k1.public_key().to_z32(), k2.public_key().to_z32());
    }

    #[test]
    fn test_pake_success() {
        let code = "7-tiger-lamp";

        let server = PakeServer::new(code);
        let client = PakeClient::new(code);

        let server_key = server.finish(client.message()).unwrap();
        let client_key = client.finish(&PakeServer::new(code).message()).unwrap();

        // both derive same key
        // (need fresh server for the message since we consumed it)
        let server2 = PakeServer::new(code);
        let client2 = PakeClient::new(code);
        let sk = server2.finish(client2.message()).unwrap();
        let ck = client2.finish(server2.message()).unwrap();
        assert_eq!(sk, ck);
    }

    #[test]
    fn test_pake_wrong_code() {
        let server = PakeServer::new("7-tiger-lamp");
        let client = PakeClient::new("8-wrong-code");

        // finish should fail
        let result = server.finish(client.message());
        assert!(result.is_err());
    }
}
