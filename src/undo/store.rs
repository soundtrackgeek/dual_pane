use std::{
    collections::HashMap,
    fs::{self, File, OpenOptions},
    io::{self, BufReader, BufWriter, Read, Write},
    sync::{
        Arc, Mutex,
        atomic::{AtomicU32, Ordering},
    },
    thread,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use windows::{
    Win32::Storage::FileSystem::{MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW},
    core::PCWSTR,
};

use super::*;

const MAGIC: &[u8; 4] = b"DPU1";
const MAX_GROUPS: usize = 24;
const MAX_BYTES: usize = 16 * 1024 * 1024;
static SERIAL: AtomicU32 = AtomicU32::new(0);

#[derive(Clone, Copy)]
#[repr(u8)]
pub enum Kind {
    Rename = 0,
    BatchRename = 1,
    Move = 2,
}

impl Kind {
    pub fn label(self) -> &'static str {
        match self {
            Self::Rename => "Rename",
            Self::BatchRename => "Batch rename",
            Self::Move => "Move",
        }
    }
}

#[derive(Clone)]
pub struct Group {
    pub id: u128,
    pub kind: Kind,
    pub items: Vec<Receipt>,
}

impl Group {
    pub fn created_ms(&self) -> u64 {
        (self.id >> 64) as u64
    }
}

pub struct Applied {
    pub checks: Vec<Check>,
    pub touched: Vec<(PathBuf, PathBuf)>,
    pub save_error: Option<String>,
}

pub struct Store {
    root: Option<PathBuf>,
    session: Mutex<Vec<Arc<Group>>>,
}

impl Store {
    pub fn new(persistent: bool) -> Self {
        let root = persistent
            .then(|| std::env::var_os("LOCALAPPDATA"))
            .flatten()
            .filter(|root| Path::new(root).is_absolute())
            .map(|root| PathBuf::from(root).join("DualPane").join("History"));
        Self {
            root,
            session: Mutex::default(),
        }
    }

    pub fn list(&self) -> io::Result<Vec<Arc<Group>>> {
        self.access(false, |groups| groups.clone())
            .map(|(groups, _)| groups)
    }

    pub fn record(&self, kind: Kind, items: Vec<Receipt>) -> io::Result<()> {
        if items.is_empty() {
            return Ok(());
        }
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis();
        let id = (time << 64)
            | (u128::from(std::process::id()) << 32)
            | u128::from(SERIAL.fetch_add(1, Ordering::Relaxed));
        let mut bytes = 0;
        let items = items
            .into_iter()
            .take(MAX_ITEMS)
            .take_while(|item| {
                bytes += item_size(item);
                bytes < MAX_BYTES - 128
            })
            .collect::<Vec<_>>();
        if items.is_empty() {
            return Ok(());
        }
        let (_, error) = self.access(true, |groups| {
            groups.insert(0, Arc::new(Group { id, kind, items }));
            trim(groups);
        })?;
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }

    pub fn apply(&self, id: u128, reviewed: &[Check]) -> io::Result<Applied> {
        let (result, save_error) = self.access(true, |groups| {
            let mut result = Applied {
                checks: Vec::new(),
                touched: Vec::new(),
                save_error: None,
            };
            let Some(index) = groups.iter().position(|group| group.id == id) else {
                return Err(io::Error::other(
                    "This operation is no longer in the saved history.",
                ));
            };
            let group = Arc::make_mut(&mut groups[index]);
            if reviewed.len() != group.items.len() {
                return Err(io::Error::other("The operation changed. Review it again."));
            }
            let mut restored = HashMap::new();
            for (receipt, prior) in group.items.iter_mut().zip(reviewed) {
                let mut state = check(receipt);
                if state == Check::Ready && *prior == Check::Ready {
                    state = match restore(receipt) {
                        Ok(proof) => {
                            result
                                .touched
                                .push((receipt.to.clone(), receipt.from.clone()));
                            if let Some(proof) = proof
                                && proof.same_payload(receipt.before)
                            {
                                restored.insert(
                                    (
                                        receipt.roots[0],
                                        receipt.from.file_name().unwrap().to_owned(),
                                        receipt.before,
                                    ),
                                    proof,
                                );
                            }
                            Check::Restored
                        }
                        Err(error) => Check::Failed(error.to_string()),
                    };
                }
                if state == Check::Restored {
                    receipt.restored = true;
                }
                result.checks.push(state);
            }
            // Keep an earlier confirmed app operation eligible when we have
            // restored exactly the version it left at this name.
            if !restored.is_empty() {
                for group in &mut groups[index + 1..] {
                    for receipt in &mut Arc::make_mut(group).items {
                        if !receipt.restored
                            && let Some(proof) = restored.get(&(
                                receipt.roots[1],
                                receipt.to.file_name().unwrap().to_owned(),
                                receipt.after,
                            ))
                        {
                            receipt.after = *proof;
                        }
                    }
                }
            }
            Ok(result)
        })?;
        let mut result = result?;
        result.save_error = save_error.map(|error| error.to_string());
        Ok(result)
    }

    fn access<T>(
        &self,
        write: bool,
        edit: impl FnOnce(&mut Vec<Arc<Group>>) -> T,
    ) -> io::Result<(T, Option<io::Error>)> {
        let Some(root) = &self.root else {
            let mut groups = self.session.lock().unwrap();
            return Ok((edit(&mut groups), None));
        };
        fs::create_dir_all(root)?;
        let _lock = lock(root)?;
        let path = root.join("undo-v1.bin");
        let mut groups = load(&path)?;
        let result = edit(&mut groups);
        let error = write.then(|| save(root, &path, &groups).err()).flatten();
        Ok((result, error))
    }
}

fn item_size(item: &Receipt) -> usize {
    180 + 2
        * (item.from.as_os_str().encode_wide().count() + item.to.as_os_str().encode_wide().count())
}

fn trim(groups: &mut Vec<Arc<Group>>) {
    let (mut items, mut bytes) = (0, 128);
    let keep = groups
        .iter()
        .take(MAX_GROUPS)
        .take_while(|group| {
            items += group.items.len();
            bytes += 32 + group.items.iter().map(item_size).sum::<usize>();
            items <= MAX_ITEMS && bytes <= MAX_BYTES
        })
        .count();
    groups.truncate(keep);
}

fn lock(root: &Path) -> io::Result<File> {
    let path = root.join("lock");
    for attempt in 0..100 {
        match OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .share_mode(0)
            .open(&path)
        {
            Ok(lock) => return Ok(lock),
            Err(error) if error.raw_os_error() == Some(32) && attempt < 99 => {
                thread::sleep(Duration::from_millis(20))
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "The undo history is invalid.")
}

fn load(path: &Path) -> io::Result<Vec<Arc<Group>>> {
    let file = match File::open(path) {
        Ok(file) => file,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    if file.metadata()?.len() > MAX_BYTES as u64 {
        return Err(invalid());
    }
    let mut read = BufReader::new(file);
    if &take::<4>(&mut read)? != MAGIC {
        return Err(invalid());
    }
    let count = number(&mut read)? as usize;
    if count > MAX_GROUPS {
        return Err(invalid());
    }
    let mut groups = Vec::with_capacity(count);
    let mut rows = 0;
    for _ in 0..count {
        let id = u128::from_le_bytes(take(&mut read)?);
        let kind = match take::<1>(&mut read)?[0] {
            0 => Kind::Rename,
            1 => Kind::BatchRename,
            2 => Kind::Move,
            _ => return Err(invalid()),
        };
        let count = number(&mut read)? as usize;
        rows += count;
        if rows > MAX_ITEMS || count == 0 {
            return Err(invalid());
        }
        let mut items = Vec::with_capacity(count);
        for _ in 0..count {
            let from = read_path(&mut read)?;
            let to = read_path(&mut read)?;
            let roots = [read_root(&mut read)?, read_root(&mut read)?];
            let before = read_proof(&mut read)?;
            let after = read_proof(&mut read)?;
            let restored = match take::<1>(&mut read)?[0] {
                0 => false,
                1 => true,
                _ => return Err(invalid()),
            };
            if !before.same_payload(after) {
                return Err(invalid());
            }
            items.push(Receipt {
                from,
                to,
                roots,
                before,
                after,
                restored,
            });
        }
        groups.push(Arc::new(Group { id, kind, items }));
    }
    if read.read(&mut [0])? != 0 {
        return Err(invalid());
    }
    Ok(groups)
}

fn save(root: &Path, path: &Path, groups: &[Arc<Group>]) -> io::Result<()> {
    let stage = root.join(format!(
        "undo-{}-{}.tmp",
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    ));
    let file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&stage)?;
    let result = (|| {
        let mut write = BufWriter::new(file);
        write.write_all(MAGIC)?;
        write.write_all(&(groups.len() as u32).to_le_bytes())?;
        for group in groups {
            write.write_all(&group.id.to_le_bytes())?;
            write.write_all(&[group.kind as u8])?;
            write.write_all(&(group.items.len() as u32).to_le_bytes())?;
            for item in &group.items {
                write_path(&mut write, &item.from)?;
                write_path(&mut write, &item.to)?;
                for root in item.roots {
                    for value in [root.0, root.1, root.2] {
                        write.write_all(&value.to_le_bytes())?;
                    }
                }
                write_proof(&mut write, item.before)?;
                write_proof(&mut write, item.after)?;
                write.write_all(&[u8::from(item.restored)])?;
            }
        }
        write.flush()?;
        write.get_ref().sync_all()?;
        drop(write);
        let from = extended_path(&stage);
        let to = extended_path(path);
        unsafe {
            MoveFileExW(
                PCWSTR(from.as_ptr()),
                PCWSTR(to.as_ptr()),
                MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
            )
        }
        .map_err(io::Error::other)
    })();
    if result.is_err() {
        let _ = fs::remove_file(stage);
    }
    result
}

fn take<const N: usize>(reader: &mut impl Read) -> io::Result<[u8; N]> {
    let mut bytes = [0; N];
    reader.read_exact(&mut bytes)?;
    Ok(bytes)
}

fn number(reader: &mut impl Read) -> io::Result<u32> {
    Ok(u32::from_le_bytes(take(reader)?))
}

fn read_root(reader: &mut impl Read) -> io::Result<Identity> {
    Ok((number(reader)?, number(reader)?, number(reader)?))
}

fn read_path(reader: &mut impl Read) -> io::Result<PathBuf> {
    let count = number(reader)? as usize;
    if count > 32767 || count == 0 {
        return Err(invalid());
    }
    let mut bytes = vec![0; count * 2];
    reader.read_exact(&mut bytes)?;
    let units = bytes
        .chunks_exact(2)
        .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
        .collect::<Vec<_>>();
    let path = PathBuf::from(OsString::from_wide(&units));
    valid_path(&path).then_some(path).ok_or_else(invalid)
}

fn write_path(writer: &mut impl Write, path: &Path) -> io::Result<()> {
    let units = path.as_os_str().encode_wide();
    writer.write_all(&(units.clone().count() as u32).to_le_bytes())?;
    for unit in units {
        writer.write_all(&unit.to_le_bytes())?;
    }
    Ok(())
}

fn read_proof(reader: &mut impl Read) -> io::Result<Proof> {
    Ok(Proof {
        volume: u64::from_le_bytes(take(reader)?),
        id: take(reader)?,
        created: u64::from_le_bytes(take(reader)?),
        modified: u64::from_le_bytes(take(reader)?),
        changed: i64::from_le_bytes(take(reader)?),
        bytes: u64::from_le_bytes(take(reader)?),
        attributes: number(reader)?,
    })
}

fn write_proof(writer: &mut impl Write, proof: Proof) -> io::Result<()> {
    writer.write_all(&proof.volume.to_le_bytes())?;
    writer.write_all(&proof.id)?;
    writer.write_all(&proof.created.to_le_bytes())?;
    writer.write_all(&proof.modified.to_le_bytes())?;
    writer.write_all(&proof.changed.to_le_bytes())?;
    writer.write_all(&proof.bytes.to_le_bytes())?;
    writer.write_all(&proof.attributes.to_le_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_undo_chain_and_rechecks_a_destination_created_after_review() -> io::Result<()> {
        struct Fixture(PathBuf);
        impl Drop for Fixture {
            fn drop(&mut self) {
                let _ = fs::remove_dir_all(&self.0);
            }
        }
        let time = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path =
            std::env::temp_dir().join(format!("dual-pane-undo-{}-{time}", std::process::id()));
        fs::create_dir(&path)?;
        let fixture = Fixture(path);
        let folder = &fixture.0;
        let history = || Store {
            root: Some(folder.join("history")),
            session: Mutex::default(),
        };
        let original = folder.join("original.txt");
        let middle = folder.join("middle.txt");
        let final_name = folder.join("final.txt");
        fs::write(&original, b"original payload")?;
        for (source, target) in [(&original, &middle), (&middle, &final_name)] {
            let candidate = Candidate::capture(source, target).expect("recordable source");
            fs::rename(source, target)?;
            let receipt = candidate.confirm(target).expect("confirmed rename");
            history().record(Kind::Rename, vec![receipt])?;
        }
        let store = history();
        let groups = store.list()?;
        assert_eq!(groups.len(), 2);
        let checks = review(&groups[0]);
        assert!(checks == [Check::Ready]);
        fs::write(&middle, b"occupied after review")?;
        let failed = store.apply(groups[0].id, &checks)?;
        assert!(failed.checks == [Check::OriginalOccupied]);
        assert_eq!(fs::read(&middle)?, b"occupied after review");
        assert_eq!(fs::read(&final_name)?, b"original payload");
        fs::remove_file(&middle)?;

        let applied = history().apply(groups[0].id, &checks)?;
        assert!(applied.checks == [Check::Restored]);
        assert_eq!(fs::read(&middle)?, b"original payload");
        let groups = history().list()?;
        assert!(groups[0].items[0].restored());
        let checks = review(&groups[1]);
        assert!(checks == [Check::Ready], "{}", checks[0].label());
        let applied = history().apply(groups[1].id, &checks)?;
        assert!(applied.checks == [Check::Restored]);
        assert_eq!(fs::read(&original)?, b"original payload");
        assert!(
            history()
                .list()?
                .iter()
                .all(|group| group.items[0].restored())
        );
        Ok(())
    }
}
