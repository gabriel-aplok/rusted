const BLOCK: usize = 64;
const DIGEST: usize = 32;

const INIT: [u32; 8] = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
];

const K: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

#[derive(Clone)]
pub(crate) struct Sha256 {
    state: [u32; 8],
    buffer: [u8; BLOCK],
    buffered: usize,
    length: u64,
}

impl Sha256 {
    pub(crate) fn new() -> Self {
        Self {
            state: INIT,
            buffer: [0; BLOCK],
            buffered: 0,
            length: 0,
        }
    }

    pub(crate) fn update(&mut self, mut data: &[u8]) {
        self.length = self.length.wrapping_add(data.len() as u64);

        if self.buffered > 0 {
            let take = (BLOCK - self.buffered).min(data.len());
            self.buffer[self.buffered..self.buffered + take].copy_from_slice(&data[..take]);
            self.buffered += take;
            data = &data[take..];

            if self.buffered == BLOCK {
                let block = self.buffer;
                self.compress(&block);
                self.buffered = 0;
            }
        }

        while data.len() >= BLOCK {
            let mut block = [0u8; BLOCK];
            block.copy_from_slice(&data[..BLOCK]);
            self.compress(&block);
            data = &data[BLOCK..];
        }

        if !data.is_empty() {
            self.buffer[..data.len()].copy_from_slice(data);
            self.buffered = data.len();
        }
    }

    pub(crate) fn finish(mut self) -> [u8; DIGEST] {
        let bit_length = self.length.wrapping_mul(8);

        self.update(&[0x80]);
        while self.buffered != BLOCK - 8 {
            self.update(&[0]);
        }
        self.update(&bit_length.to_be_bytes());

        let mut digest = [0u8; DIGEST];
        for (chunk, word) in digest.chunks_exact_mut(4).zip(self.state) {
            chunk.copy_from_slice(&word.to_be_bytes());
        }
        digest
    }

    fn compress(&mut self, block: &[u8; BLOCK]) {
        let mut schedule = [0u32; 64];
        for (index, chunk) in block.chunks_exact(4).enumerate() {
            schedule[index] = u32::from_be_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]);
        }
        for index in 16..64 {
            let s0 = schedule[index - 15].rotate_right(7)
                ^ schedule[index - 15].rotate_right(18)
                ^ (schedule[index - 15] >> 3);
            let s1 = schedule[index - 2].rotate_right(17)
                ^ schedule[index - 2].rotate_right(19)
                ^ (schedule[index - 2] >> 10);
            schedule[index] = schedule[index - 16]
                .wrapping_add(s0)
                .wrapping_add(schedule[index - 7])
                .wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for index in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let choice = (e & f) ^ ((!e) & g);
            let temp1 = h
                .wrapping_add(s1)
                .wrapping_add(choice)
                .wrapping_add(K[index])
                .wrapping_add(schedule[index]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let majority = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(majority);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }
}

#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn sha256(data: &[u8]) -> [u8; DIGEST] {
    let mut hasher = Sha256::new();
    hasher.update(data);
    hasher.finish()
}

pub(crate) fn to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";

    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::{sha256, to_hex, Sha256};

    const EMPTY: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
    const ABC: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
    const STANDARD_56: &str = "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1";
    const MILLION_A: &str = "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0";
    const ONE_MIB_PATTERN: &str =
        "631b84027d6b9e52b539c4e8373622d23032dfadc64d60af87339c9037e4f769";

    #[test]
    fn empty_matches_reference() {
        assert_eq!(to_hex(&sha256(b"")), EMPTY);
    }

    #[test]
    fn abc_matches_reference() {
        assert_eq!(to_hex(&sha256(b"abc")), ABC);
    }

    #[test]
    fn standard_56_byte_matches_reference() {
        let input = b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq";
        assert_eq!(to_hex(&sha256(input)), STANDARD_56);
    }

    #[test]
    fn block_boundaries_match_reference() {
        assert_eq!(
            to_hex(&sha256(&[b'a'; 55])),
            "9f4390f8d30c2dd92ec9f095b65e2b9ae9b0a925a5258e241c9f1e910f734318"
        );
        assert_eq!(
            to_hex(&sha256(&[b'a'; 56])),
            "b35439a4ac6f0948b6d6f9e3c6af0f5f590ce20f1bde7090ef7970686ec6738a"
        );
        assert_eq!(
            to_hex(&sha256(&[b'a'; 63])),
            "7d3e74a05d7db15bce4ad9ec0658ea98e3f06eeecf16b4c6fff2da457ddc2f34"
        );
        assert_eq!(
            to_hex(&sha256(&[b'a'; 64])),
            "ffe054fe7ae0cb6dc65c3af9b61d5209f439851db43d0ba5997337df154668eb"
        );
        assert_eq!(
            to_hex(&sha256(&[b'a'; 65])),
            "635361c48bb9eab14198e76ea8ab7f1a41685d6ad62aa9146d301d4f17eb0ae0"
        );
    }

    #[test]
    fn million_a_matches_reference() {
        let input = vec![b'a'; 1_000_000];
        assert_eq!(to_hex(&sha256(&input)), MILLION_A);
    }

    #[test]
    fn one_mebibyte_pattern_matches_reference() {
        let input: Vec<u8> = (0..1024 * 1024).map(|index| (index % 251) as u8).collect();
        assert_eq!(to_hex(&sha256(&input)), ONE_MIB_PATTERN);
    }

    #[test]
    fn incremental_update_matches_one_shot() {
        let input: Vec<u8> = (0..1024 * 1024).map(|index| (index % 251) as u8).collect();
        let mut hasher = Sha256::new();
        for chunk in input.chunks(7) {
            hasher.update(chunk);
        }
        assert_eq!(hasher.finish(), sha256(&input));
    }

    #[test]
    fn hex_encoding() {
        assert_eq!(to_hex(&[0x00, 0x0f, 0xff]), "000fff");
    }
}
