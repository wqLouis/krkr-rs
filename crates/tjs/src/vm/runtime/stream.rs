//! The streams a save and a load read and write.
//!
//! A save member writes a file and a load member reads one. The file is
//! reached through **one seam**: a host installs a [`StreamHost`], which
//! answers an open stream for a name and a mode. The object model asks for one
//! of the four streams it needs — a text or a binary one, for reading or for
//! writing — and never opens anything itself: nothing in this module names a
//! path, a directory or a platform service.
//!
//! # Why a factory and not an open file
//!
//! The name and the mode are script values. The mode selects the container a
//! save writes (the cipher digit, the compression flag, the `oN` offset) and
//! the name is resolved by the storage the host owns — an archive member, a
//! media-prefixed name, a relative path under the game's data directory. Both
//! decisions belong to the host, so the object model hands the two texts over
//! unchanged and the host answers an open stream, positioned however the mode
//! says.
//!
//! The four streams are the four the members actually use:
//!
//! | member | stream |
//! |---|---|
//! | `Array.save` | text write |
//! | `Array.load` | text read |
//! | `Array.saveStruct` | text write, or binary write for a mode holding `b` |
//! | `Array.loadStruct` | binary read |
//!
//! # The installation
//!
//! The host is installed once, by the composition root, and the members find
//! it there. It is the one piece of process state this runtime keeps, and it is
//! the seam itself: a member call carries no host argument, and a script
//! reaches a member through a dispatch that has no room for one. Installing a
//! host answers the one it replaced, so a caller that installs a host for one
//! check can put the previous one back.
//!
//! A member that needs a stream and finds no host installed refuses with
//! [`StreamError::NotInstalled`] rather than writing nowhere: a missing host is
//! a defect of the composition root, not an answer a script can read.
//!
//! [`MemoryStreams`] is an in-memory host: every stream it opens reads and
//! writes a table this process owns, so a check drives the members without a
//! file and reads back exactly what they wrote.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, MutexGuard};

use super::string::TjsString;

/// A text stream a load reads in one piece.
pub trait TextReadStream {
    /// Reads the whole stream.
    ///
    /// # Errors
    ///
    /// [`StreamError::Read`] when the text cannot be read.
    fn read_all(&mut self) -> Result<TjsString, StreamError>;
}

/// A text stream a save appends to.
pub trait TextWriteStream {
    /// Appends `text`.
    ///
    /// # Errors
    ///
    /// [`StreamError::Write`] when the text cannot be written.
    fn write(&mut self, text: &TjsString) -> Result<(), StreamError>;
}

/// A binary stream a structured save writes.
pub trait BinaryWriteStream {
    /// Appends `bytes`.
    ///
    /// # Errors
    ///
    /// [`StreamError::Write`] when the bytes cannot be written.
    fn write(&mut self, bytes: &[u8]) -> Result<(), StreamError>;
}

/// A binary stream a structured load reads.
pub trait BinaryReadStream {
    /// The size of the whole stream, from its first byte.
    ///
    /// # Errors
    ///
    /// [`StreamError::Seek`] when the size cannot be read.
    fn size(&self) -> Result<u64, StreamError>;

    /// The position the next read takes its byte from.
    ///
    /// # Errors
    ///
    /// [`StreamError::Seek`] when the position cannot be read.
    fn position(&self) -> Result<u64, StreamError>;

    /// Reads into `bytes`, answering how many bytes were read.
    ///
    /// A shorter answer is not an error: the callers that need a whole buffer
    /// compare the count themselves.
    ///
    /// # Errors
    ///
    /// [`StreamError::Read`] when the stream cannot be read.
    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, StreamError>;
}

/// The stream factories a host supplies.
///
/// One method per stream kind, and each takes the name and the mode the script
/// gave, unchanged: what the mode selects and what the name names is the
/// host's business.
pub trait StreamHost {
    /// Opens a text stream for reading.
    ///
    /// # Errors
    ///
    /// [`StreamError::CannotOpen`] when the host cannot place the name.
    fn open_text_read(
        &self,
        name: &TjsString,
        mode: &TjsString,
    ) -> Result<Box<dyn TextReadStream>, StreamError>;

    /// Opens a text stream for writing.
    ///
    /// # Errors
    ///
    /// [`StreamError::CannotOpen`] when the host cannot create the name.
    fn open_text_write(
        &self,
        name: &TjsString,
        mode: &TjsString,
    ) -> Result<Box<dyn TextWriteStream>, StreamError>;

    /// Opens a binary stream for reading.
    ///
    /// # Errors
    ///
    /// [`StreamError::CannotOpen`] when the host cannot place the name.
    fn open_binary_read(
        &self,
        name: &TjsString,
        mode: &TjsString,
    ) -> Result<Box<dyn BinaryReadStream>, StreamError>;

    /// Opens a binary stream for writing.
    ///
    /// # Errors
    ///
    /// [`StreamError::CannotOpen`] when the host cannot create the name.
    fn open_binary_write(
        &self,
        name: &TjsString,
        mode: &TjsString,
    ) -> Result<Box<dyn BinaryWriteStream>, StreamError>;
}

/// The failure of a stream.
///
/// The messages are the ones a script reads when a save or a load fails, so
/// the text of each one is the engine's and not a summary of it.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[non_exhaustive]
pub enum StreamError {
    /// The host cannot open the name.
    #[error("Cannot open storage {name}")]
    CannotOpen {
        /// The name the member asked for.
        name: TjsString,
    },

    /// A read did not answer the bytes it was asked for.
    #[error("Read error")]
    Read,

    /// A write did not take the bytes it was given.
    #[error("Write error")]
    Write,

    /// A stream cannot be positioned.
    #[error("Seek error")]
    Seek,

    /// No host is installed.
    #[error("the stream host is not installed")]
    NotInstalled,
}

/// An installed stream host.
pub type SharedHost = Arc<dyn StreamHost + Send + Sync>;

/// The host the members reach, once a composition root installs one.
static HOST: Mutex<Option<SharedHost>> = Mutex::new(None);

/// The lock on the installed host.
///
/// A panic while the host was held leaves the host itself intact — it is a
/// table of factories — so the value is taken back rather than the call
/// failing.
fn slot() -> MutexGuard<'static, Option<SharedHost>> {
    match HOST.lock() {
        Ok(guard) => guard,
        Err(poisoned) => poisoned.into_inner(),
    }
}

/// Installs `host` and answers the one it replaced.
pub fn install(host: SharedHost) -> Option<SharedHost> {
    slot().replace(host)
}

/// Removes the installed host and answers it.
pub fn uninstall() -> Option<SharedHost> {
    slot().take()
}

/// The installed host, when there is one.
pub fn host() -> Option<SharedHost> {
    slot().clone()
}

/// The installed host, or the refusal a member answers when there is none.
///
/// # Errors
///
/// [`StreamError::NotInstalled`] when no host is installed.
pub fn installed() -> Result<SharedHost, StreamError> {
    host().ok_or(StreamError::NotInstalled)
}

/// The one entry the object model needs: a host, or the typed refusal.
///
/// # Errors
///
/// [`StreamError::NotInstalled`] when no host is installed.
pub fn open_text_read(
    name: &TjsString,
    mode: &TjsString,
) -> Result<Box<dyn TextReadStream>, StreamError> {
    installed()?.open_text_read(name, mode)
}

/// [`StreamHost::open_text_write`] on the installed host.
///
/// # Errors
///
/// [`StreamError::NotInstalled`] when no host is installed, and the host's own
/// refusal otherwise.
pub fn open_text_write(
    name: &TjsString,
    mode: &TjsString,
) -> Result<Box<dyn TextWriteStream>, StreamError> {
    installed()?.open_text_write(name, mode)
}

/// [`StreamHost::open_binary_read`] on the installed host.
///
/// # Errors
///
/// [`StreamError::NotInstalled`] when no host is installed, and the host's own
/// refusal otherwise.
pub fn open_binary_read(
    name: &TjsString,
    mode: &TjsString,
) -> Result<Box<dyn BinaryReadStream>, StreamError> {
    installed()?.open_binary_read(name, mode)
}

/// [`StreamHost::open_binary_write`] on the installed host.
///
/// # Errors
///
/// [`StreamError::NotInstalled`] when no host is installed, and the host's own
/// refusal otherwise.
pub fn open_binary_write(
    name: &TjsString,
    mode: &TjsString,
) -> Result<Box<dyn BinaryWriteStream>, StreamError> {
    installed()?.open_binary_write(name, mode)
}

/// The store [`MemoryStreams`] keeps.
#[derive(Debug, Default)]
struct MemoryStore {
    /// The text each name holds.
    text: BTreeMap<TjsString, TjsString>,
    /// The bytes each name holds.
    bytes: BTreeMap<TjsString, Vec<u8>>,
}

/// An in-memory stream host.
///
/// A name holds a text or a block of bytes. Opening a stream for writing
/// starts the name's entry empty, as a write stream opened for a fresh file
/// does; opening one for reading answers what is stored. The store is shared
/// and cloned into the handle, so a caller reads back what a save wrote.
#[derive(Clone, Debug, Default)]
pub struct MemoryStreams {
    /// The entries, shared between the handle and every stream it opened.
    store: Arc<Mutex<MemoryStore>>,
}

impl MemoryStreams {
    /// An empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// The entries, through the one lock.
    fn store(&self) -> MutexGuard<'_, MemoryStore> {
        match self.store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        }
    }

    /// Stores `text` under `name`.
    pub fn set_text(&self, name: &str, text: &str) {
        self.store()
            .text
            .insert(TjsString::from(name), TjsString::from(text));
    }

    /// The text `name` holds.
    pub fn text(&self, name: &str) -> Option<TjsString> {
        self.store().text.get(&TjsString::from(name)).cloned()
    }

    /// Stores `bytes` under `name`.
    pub fn set_bytes(&self, name: &str, bytes: &[u8]) {
        self.store()
            .bytes
            .insert(TjsString::from(name), bytes.to_vec());
    }

    /// The bytes `name` holds.
    pub fn bytes(&self, name: &str) -> Option<Vec<u8>> {
        self.store().bytes.get(&TjsString::from(name)).cloned()
    }
}

/// A text save writing into the store.
struct MemoryTextWrite {
    /// The store the text goes to.
    store: Arc<Mutex<MemoryStore>>,
    /// The name the text is stored under.
    name: TjsString,
}

impl TextWriteStream for MemoryTextWrite {
    fn write(&mut self, text: &TjsString) -> Result<(), StreamError> {
        let mut store = match self.store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        let entry = store
            .text
            .entry(self.name.clone())
            .or_insert_with(TjsString::new);
        *entry = entry.concat(text);
        Ok(())
    }
}

/// A text load reading what the store holds.
struct MemoryTextRead {
    /// The text the name held when the stream opened.
    text: TjsString,
}

impl TextReadStream for MemoryTextRead {
    fn read_all(&mut self) -> Result<TjsString, StreamError> {
        Ok(self.text.clone())
    }
}

/// A binary save writing into the store.
struct MemoryBinaryWrite {
    /// The store the bytes go to.
    store: Arc<Mutex<MemoryStore>>,
    /// The name the bytes are stored under.
    name: TjsString,
}

impl BinaryWriteStream for MemoryBinaryWrite {
    fn write(&mut self, bytes: &[u8]) -> Result<(), StreamError> {
        let mut store = match self.store.lock() {
            Ok(guard) => guard,
            Err(poisoned) => poisoned.into_inner(),
        };
        store
            .bytes
            .entry(self.name.clone())
            .or_default()
            .extend_from_slice(bytes);
        Ok(())
    }
}

/// A binary load reading what the store held when the stream opened.
struct MemoryBinaryRead {
    /// The bytes the name held when the stream opened.
    bytes: Vec<u8>,
    /// The position the next read takes its byte from.
    position: usize,
}

impl BinaryReadStream for MemoryBinaryRead {
    fn size(&self) -> Result<u64, StreamError> {
        u64::try_from(self.bytes.len()).map_err(|_| StreamError::Seek)
    }

    fn position(&self) -> Result<u64, StreamError> {
        u64::try_from(self.position).map_err(|_| StreamError::Seek)
    }

    fn read(&mut self, bytes: &mut [u8]) -> Result<usize, StreamError> {
        let remaining = self.bytes.len().saturating_sub(self.position);
        let count = remaining.min(bytes.len());
        let end = self.position + count;
        bytes[..count].copy_from_slice(&self.bytes[self.position..end]);
        self.position = end;
        Ok(count)
    }
}

impl StreamHost for MemoryStreams {
    fn open_text_read(
        &self,
        name: &TjsString,
        _mode: &TjsString,
    ) -> Result<Box<dyn TextReadStream>, StreamError> {
        match self.store().text.get(name).cloned() {
            Some(text) => Ok(Box::new(MemoryTextRead { text })),
            None => Err(StreamError::CannotOpen { name: name.clone() }),
        }
    }

    fn open_text_write(
        &self,
        name: &TjsString,
        _mode: &TjsString,
    ) -> Result<Box<dyn TextWriteStream>, StreamError> {
        self.store().text.insert(name.clone(), TjsString::new());
        Ok(Box::new(MemoryTextWrite {
            store: Arc::clone(&self.store),
            name: name.clone(),
        }))
    }

    fn open_binary_read(
        &self,
        name: &TjsString,
        _mode: &TjsString,
    ) -> Result<Box<dyn BinaryReadStream>, StreamError> {
        match self.store().bytes.get(name).cloned() {
            Some(bytes) => Ok(Box::new(MemoryBinaryRead { bytes, position: 0 })),
            None => Err(StreamError::CannotOpen { name: name.clone() }),
        }
    }

    fn open_binary_write(
        &self,
        name: &TjsString,
        _mode: &TjsString,
    ) -> Result<Box<dyn BinaryWriteStream>, StreamError> {
        self.store().bytes.insert(name.clone(), Vec::new());
        Ok(Box::new(MemoryBinaryWrite {
            store: Arc::clone(&self.store),
            name: name.clone(),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{MemoryStreams, StreamError, StreamHost, install, uninstall};
    use crate::vm::runtime::string::TjsString;

    /// A name.
    fn name(text: &str) -> TjsString {
        TjsString::from(text)
    }

    /// The mode a save writes with when the script gives none.
    fn mode() -> TjsString {
        TjsString::new()
    }

    /// A text written through the store and read back.
    ///
    // reference/krkr2/cpp/core/tjs2/tjsArray.cpp:394-434
    #[test]
    fn a_text_stream_round_trips_through_the_store() {
        let streams = MemoryStreams::new();
        let mut write = streams
            .open_text_write(&name("save.txt"), &mode())
            .expect("the store opens a text write");
        write
            .write(&TjsString::from("one\r\n"))
            .expect("the text is written");
        write
            .write(&TjsString::from("two"))
            .expect("the text is written");
        drop(write);
        assert_eq!(
            streams.text("save.txt"),
            Some(TjsString::from("one\r\ntwo"))
        );
        let mut read = streams
            .open_text_read(&name("save.txt"), &mode())
            .expect("the store opens a text read");
        assert_eq!(
            read.read_all().expect("the text is read"),
            TjsString::from("one\r\ntwo")
        );
    }

    /// A binary written through the store and read back byte by byte.
    ///
    // reference/krkr2/cpp/core/tjs2/tjsBinarySerializer.cpp:1-40
    #[test]
    fn a_binary_stream_round_trips_through_the_store() {
        let streams = MemoryStreams::new();
        let mut write = streams
            .open_binary_write(&name("save.bin"), &mode())
            .expect("the store opens a binary write");
        write.write(&[0x98]).expect("the byte is written");
        write.write(&[0x01, 0x02]).expect("the bytes are written");
        drop(write);
        assert_eq!(streams.bytes("save.bin"), Some(vec![0x98, 0x01, 0x02]));

        let mut read = streams
            .open_binary_read(&name("save.bin"), &mode())
            .expect("the store opens a binary read");
        assert_eq!(read.size(), Ok(3));
        assert_eq!(read.position(), Ok(0));
        let mut buffer = [0u8; 2];
        assert_eq!(read.read(&mut buffer), Ok(2));
        assert_eq!(buffer, [0x98, 0x01]);
        assert_eq!(read.position(), Ok(2));
        let mut rest = [0u8; 4];
        assert_eq!(read.read(&mut rest), Ok(1));
        assert_eq!(rest[0], 0x02);
    }

    /// A name the store does not hold is the host's open failure, and its
    /// message is the one a script reads.
    ///
    // reference/krkr2/cpp/core/base/StorageIntf.cpp:1140-1148
    #[test]
    fn a_missing_name_is_the_open_failure() {
        let streams = MemoryStreams::new();
        let error = match streams.open_text_read(&name("absent.txt"), &mode()) {
            Ok(_opened) => panic!("the store holds no such name"),
            Err(error) => error,
        };
        assert_eq!(
            error,
            StreamError::CannotOpen {
                name: name("absent.txt")
            }
        );
        assert_eq!(error.to_string(), "Cannot open storage absent.txt");
        let unopened = match streams.open_binary_read(&name("absent.bin"), &mode()) {
            Ok(_opened) => panic!("the store holds no such name"),
            Err(error) => error,
        };
        assert_eq!(unopened.to_string(), "Cannot open storage absent.bin");
    }

    /// An installed host is what a member reaches, and the one it replaced
    /// comes back so it can be put in place again.
    ///
    // reference/krkr2/cpp/core/tjs2/tjs.cpp:620-640
    #[test]
    fn an_installed_host_is_replaced_and_restored() {
        let first = MemoryStreams::new();
        let second = MemoryStreams::new();
        let previous = install(std::sync::Arc::new(first.clone()));
        let replaced = install(std::sync::Arc::new(second.clone()));
        assert!(replaced.is_some(), "the first host comes back");
        let taken = uninstall();
        assert!(taken.is_some(), "the second host comes back");
        if let Some(previous) = previous {
            install(previous);
        }
        uninstall();
    }
}
