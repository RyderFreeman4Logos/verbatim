use std::io::{self, Read};

use sha2::{Digest, Sha256};

use super::update_reader;

struct PatternReader {
    remaining: usize,
    max_request: usize,
}

impl Read for PatternReader {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        self.max_request = self.max_request.max(buffer.len());
        let read = self.remaining.min(buffer.len());
        buffer[..read].fill(b'x');
        self.remaining -= read;
        Ok(read)
    }
}

#[test]
fn file_hash_reader_is_bounded_compatible_and_size_checked() {
    let length = 2 * 64 * 1024 + 17;
    let mut reader = PatternReader {
        remaining: length,
        max_request: 0,
    };
    let mut actual = Sha256::new();
    update_reader(&mut reader, &mut actual, length as u64, true).unwrap();

    let mut expected = Sha256::new();
    expected.update((length as u64).to_be_bytes());
    expected.update(vec![b'x'; length]);
    assert_eq!(actual.finalize(), expected.finalize());
    assert!(reader.max_request <= 64 * 1024);

    let mut changed = PatternReader {
        remaining: length + 1,
        max_request: 0,
    };
    let error = update_reader(&mut changed, &mut Sha256::new(), length as u64, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("size changed while hashing"), "{error}");

    let mut shortened = PatternReader {
        remaining: length - 1,
        max_request: 0,
    };
    let error = update_reader(&mut shortened, &mut Sha256::new(), length as u64, false)
        .unwrap_err()
        .to_string();
    assert!(error.contains("size changed while hashing"), "{error}");
}
