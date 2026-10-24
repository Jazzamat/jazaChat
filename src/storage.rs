use libp2p::identity::Keypair;
use std::{
    fs::{self, OpenOptions},
    io::{self, Write},
    path::Path,
};

pub fn identity(directory: &Path) -> io::Result<Keypair> {
    fs::create_dir_all(directory)?;
    let path = directory.join("identity.key");
    if path.exists() {
        let bytes = fs::read(path)?;
        return Keypair::from_protobuf_encoding(&bytes).map_err(io::Error::other);
    }
    let key = Keypair::generate_ed25519();
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(&path) {
        Ok(mut file) => {
            file.write_all(&key.to_protobuf_encoding().map_err(io::Error::other)?)?;
            file.sync_all()?;
            Ok(key)
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            Keypair::from_protobuf_encoding(&fs::read(path)?).map_err(io::Error::other)
        }
        Err(error) => Err(error),
    }
}

pub fn load_contacts(directory: &Path) -> io::Result<Vec<String>> {
    match fs::read(directory.join("peers.json")) {
        Ok(bytes) if bytes.len() <= 256 * 1024 => {
            serde_json::from_slice(&bytes).map_err(io::Error::other)
        }
        Ok(_) => Err(io::Error::other("Saved contacts file is too large")),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(Vec::new()),
        Err(e) => Err(e),
    }
}
pub fn save_contacts(directory: &Path, contacts: &[String]) -> io::Result<()> {
    let temp = directory.join("peers.json.tmp");
    fs::write(&temp, serde_json::to_vec_pretty(contacts)?)?;
    fs::rename(temp, directory.join("peers.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_survives_restart_and_corruption_is_not_overwritten() {
        let path = std::env::temp_dir().join(format!("jazachat-{}", libp2p::PeerId::random()));
        let first = identity(&path).unwrap().public().to_peer_id();
        assert_eq!(first, identity(&path).unwrap().public().to_peer_id());
        save_contacts(&path, &["example".into()]).unwrap();
        assert_eq!(load_contacts(&path).unwrap(), vec!["example"]);
        fs::write(path.join("identity.key"), b"broken").unwrap();
        assert!(identity(&path).is_err());
        fs::remove_dir_all(path).unwrap();
    }
}
