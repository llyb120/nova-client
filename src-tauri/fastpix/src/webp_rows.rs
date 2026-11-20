//! Lossless WebP that can replace individual rows.
//!
//! A pointer move only changes the rows under the old and new sword tips. VP8L with a vertical
//! predictor mixes those rows into the rest of the frame, so the whole image would be encoded
//! again. This writer uses subtract-green only, keeps each row's bits, and on a later move
//! rewrites just the dirty rows. Huffman trees reserve every symbol, so a color that was hidden
//! under the sword still has a code.
//!
//! The tree layout matches the VP8L bitstream used by image-webp 0.2 (MIT OR Apache-2.0).

use std::collections::BinaryHeap;
use std::sync::Arc;

struct Bits {
    data: Vec<u8>,
    nbits: u64,
}

impl Bits {
    fn new() -> Self {
        Self { data: Vec::new(), nbits: 0 }
    }

    #[inline(always)]
    fn write_bits(&mut self, mut bits: u64, mut n: u8) {
        debug_assert!(n <= 64);
        while n > 0 {
            let used = (self.nbits % 8) as u8;
            let take = n.min(8 - used);
            let chunk = (bits & ((1u64 << take) - 1)) as u8;
            if used == 0 {
                self.data.push(chunk);
            } else {
                let last = self.data.len() - 1;
                self.data[last] |= chunk << used;
            }
            bits >>= take;
            n -= take;
            self.nbits += u64::from(take);
        }
    }

    fn append(&mut self, other: &Bits) {
        if other.nbits == 0 {
            return;
        }
        let full = (other.nbits / 8) as usize;
        if self.nbits % 8 == 0 {
            self.data.extend_from_slice(&other.data[..full]);
            self.nbits += other.nbits & !7;
            let rem = (other.nbits % 8) as u8;
            if rem > 0 {
                self.write_bits(u64::from(other.data[full]), rem);
            }
        } else {
            for &byte in &other.data[..full] {
                self.write_bits(u64::from(byte), 8);
            }
            let rem = (other.nbits % 8) as u8;
            if rem > 0 {
                self.write_bits(u64::from(other.data[full]), rem);
            }
        }
    }
}

struct Tables {
    len0: [u8; 256],
    code0: [u16; 256],
    len1: [u8; 280],
    code1: [u16; 280],
    len2: [u8; 256],
    code2: [u16; 256],
    len3: [u8; 256],
    code3: [u16; 256],
}

/// Cached lossless frame. `patch` rewrites only the rows touched by `dirty` rectangles.
pub struct WebpRows {
    width: u32,
    height: u32,
    prefix: Arc<Bits>,
    tables: Arc<Tables>,
    rows: Vec<Arc<Bits>>,
}

impl WebpRows {
    pub fn same_size(&self, width: u32, height: u32) -> bool {
        self.width == width && self.height == height
    }

    pub fn encode(rgba: &[u8], width: u32, height: u32) -> Result<(Vec<u8>, Self), String> {
        check(rgba, width, height)?;
        let mut freq0 = [0u32; 256];
        let mut freq1 = [0u32; 280];
        let mut freq2 = [0u32; 256];
        let mut freq3 = [0u32; 256];
        count(rgba, width, height, &mut freq0, &mut freq1, &mut freq2, &mut freq3);
        reserve(&mut freq0);
        reserve(&mut freq1);
        reserve(&mut freq2);
        reserve(&mut freq3);

        let mut prefix = Bits::new();
        write_header(&mut prefix, width, height);
        let mut tables = Tables {
            len0: [0; 256], code0: [0; 256],
            len1: [0; 280], code1: [0; 280],
            len2: [0; 256], code2: [0; 256],
            len3: [0; 256], code3: [0; 256],
        };
        write_huffman_tree(&mut prefix, &freq1, &mut tables.len1, &mut tables.code1)?;
        write_huffman_tree(&mut prefix, &freq0, &mut tables.len0, &mut tables.code0)?;
        write_huffman_tree(&mut prefix, &freq2, &mut tables.len2, &mut tables.code2)?;
        write_huffman_tree(&mut prefix, &freq3, &mut tables.len3, &mut tables.code3)?;
        write_single_entry(&mut prefix, 1)?;

        let tables = Arc::new(tables);
        let mut rows = Vec::with_capacity(height as usize);
        let stride = width as usize * 4;
        for y in 0..height as usize {
            rows.push(Arc::new(encode_row(&rgba[y * stride..][..stride], width, &tables)?));
        }
        let prefix = Arc::new(prefix);
        let bytes = assemble(&prefix, &rows);
        Ok((bytes, Self { width, height, prefix, tables, rows }))
    }

    pub fn patch(&self, rgba: &[u8], dirty: &[(u32, u32, u32, u32)]) -> Result<(Vec<u8>, Self), String> {
        check(rgba, self.width, self.height)?;
        let mut rows = self.rows.clone();
        let mut touched = vec![false; self.height as usize];
        for &(x, y, w, h) in dirty {
            if w == 0 || h == 0 || x >= self.width || y >= self.height {
                continue;
            }
            let y1 = y.saturating_add(h).min(self.height) as usize;
            for flag in &mut touched[y as usize..y1] {
                *flag = true;
            }
        }
        let stride = self.width as usize * 4;
        for (y, flag) in touched.iter().enumerate() {
            if *flag {
                rows[y] = Arc::new(encode_row(&rgba[y * stride..][..stride], self.width, &self.tables)?);
            }
        }
        let bytes = assemble(&self.prefix, &rows);
        Ok((bytes, Self {
            width: self.width,
            height: self.height,
            prefix: Arc::clone(&self.prefix),
            tables: Arc::clone(&self.tables),
            rows,
        }))
    }
}

fn check(rgba: &[u8], width: u32, height: u32) -> Result<(), String> {
    if width == 0 || height == 0 || width > 16383 || height > 16383 {
        return Err("图像尺寸超出无损 WebP".into());
    }
    if rgba.len() != width as usize * height as usize * 4 {
        return Err("像素缓冲与尺寸不一致".into());
    }
    Ok(())
}

fn reserve(freq: &mut [u32]) {
    for value in freq {
        *value = value.saturating_add(1);
    }
}

fn count(rgba: &[u8], width: u32, height: u32, f0: &mut [u32; 256], f1: &mut [u32; 280], f2: &mut [u32; 256], f3: &mut [u32; 256]) {
    let w = width as usize;
    let stride = w * 4;
    for y in 0..height as usize {
        let row = &rgba[y * stride..][..stride];
        let mut x = 0;
        while x < w {
            let px = &row[x * 4..x * 4 + 4];
            let g = px[1];
            f0[px[0].wrapping_sub(g) as usize] += 1;
            f1[g as usize] += 1;
            f2[px[2].wrapping_sub(g) as usize] += 1;
            f3[px[3] as usize] += 1;
            let mut run = 0usize;
            let mut j = x + 1;
            while run < 4096 && j < w && &row[j * 4..j * 4 + 4] == px {
                run += 1;
                j += 1;
            }
            if run > 0 {
                let symbol = run_symbol(run).0;
                f1[symbol] += 1;
            }
            x = j;
        }
    }
}

fn encode_row(row: &[u8], width: u32, tables: &Tables) -> Result<Bits, String> {
    let w = width as usize;
    let mut bits = Bits::new();
    let mut x = 0;
    while x < w {
        let px = &row[x * 4..x * 4 + 4];
        let g = px[1] as usize;
        let r = px[0].wrapping_sub(px[1]) as usize;
        let b = px[2].wrapping_sub(px[1]) as usize;
        let a = px[3] as usize;
        write_sym(&mut bits, tables.code1[g], tables.len1[g])?;
        write_sym(&mut bits, tables.code0[r], tables.len0[r])?;
        write_sym(&mut bits, tables.code2[b], tables.len2[b])?;
        write_sym(&mut bits, tables.code3[a], tables.len3[a])?;
        let mut run = 0usize;
        let mut j = x + 1;
        while run < 4096 && j < w && &row[j * 4..j * 4 + 4] == px {
            run += 1;
            j += 1;
        }
        if run > 0 {
            let (symbol, extra, extra_bits) = run_symbol(run);
            write_sym(&mut bits, tables.code1[symbol], tables.len1[symbol])?;
            if extra_bits > 0 {
                bits.write_bits(u64::from(extra), extra_bits);
            }
        }
        x = j;
    }
    Ok(bits)
}

/// (green-alphabet index, extra bits value, extra bit count) for a copy of `run` pixels.
fn run_symbol(run: usize) -> (usize, u16, u8) {
    if run <= 4 {
        (256 + run - 1, 0, 0)
    } else {
        let (symbol, extra_bits) = length_to_symbol(run as u16);
        let extra = ((run as u16).wrapping_sub(1)) & ((1u16 << extra_bits) - 1);
        (256 + symbol as usize, extra, extra_bits)
    }
}

fn write_sym(bits: &mut Bits, code: u16, len: u8) -> Result<(), String> {
    if len == 0 {
        return Err("霍夫曼表缺少符号".into());
    }
    bits.write_bits(u64::from(code), len);
    Ok(())
}

fn write_header(bits: &mut Bits, width: u32, height: u32) {
    bits.write_bits(0x2f, 8);
    bits.write_bits(u64::from(width - 1), 14);
    bits.write_bits(u64::from(height - 1), 14);
    bits.write_bits(1, 1);
    bits.write_bits(0, 3);
    bits.write_bits(0b101, 3);
    bits.write_bits(0, 1);
    bits.write_bits(0, 1);
    bits.write_bits(0, 1);
}

fn assemble(prefix: &Bits, rows: &[Arc<Bits>]) -> Vec<u8> {
    let mut all = Bits::new();
    all.data.reserve(prefix.data.len() + rows.iter().map(|r| r.data.len()).sum::<usize>() + 16);
    all.append(prefix);
    for row in rows {
        all.append(row);
    }
    wrap(&all.data)
}

fn wrap(frame: &[u8]) -> Vec<u8> {
    let pad = frame.len() % 2;
    let mut out = Vec::with_capacity(frame.len() + 21);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&((frame.len() + 12 + pad) as u32).to_le_bytes());
    out.extend_from_slice(b"WEBP");
    out.extend_from_slice(b"VP8L");
    out.extend_from_slice(&(frame.len() as u32).to_le_bytes());
    out.extend_from_slice(frame);
    if pad == 1 {
        out.push(0);
    }
    out
}

fn write_single_entry(bits: &mut Bits, symbol: u16) -> Result<(), String> {
    bits.write_bits(1, 2);
    if symbol <= 1 {
        bits.write_bits(0, 1);
        bits.write_bits(u64::from(symbol), 1);
    } else {
        bits.write_bits(1, 1);
        bits.write_bits(u64::from(symbol), 8);
    }
    Ok(())
}

const fn length_to_symbol(len: u16) -> (u16, u8) {
    let len = len - 1;
    let highest_bit = len.ilog2() as u16;
    let second_highest_bit = (len >> (highest_bit - 1)) & 1;
    let extra_bits = highest_bit - 1;
    let symbol = 2 * highest_bit + second_highest_bit;
    (symbol, extra_bits as u8)
}

fn build_huffman_tree(frequencies: &[u32], lengths: &mut [u8], codes: &mut [u16], length_limit: u8) -> Result<bool, String> {
    if frequencies.iter().filter(|&&f| f > 0).count() <= 1 {
        lengths.fill(0);
        codes.fill(0);
        return Ok(false);
    }

    #[derive(Eq, PartialEq, Copy, Clone)]
    struct Item(u32, u16);
    impl Ord for Item {
        fn cmp(&self, other: &Self) -> std::cmp::Ordering {
            other.0.cmp(&self.0)
        }
    }
    impl PartialOrd for Item {
        fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
            Some(self.cmp(other))
        }
    }

    let mut internal_nodes = Vec::new();
    let mut nodes = BinaryHeap::from_iter(
        frequencies.iter().enumerate().filter(|(_, &frequency)| frequency > 0).map(|(i, &frequency)| Item(frequency, i as u16)),
    );
    while nodes.len() > 1 {
        let Item(frequency1, index1) = nodes.pop().unwrap();
        let mut root = nodes.peek_mut().unwrap();
        internal_nodes.push((index1, root.1));
        *root = Item(frequency1 + root.0, internal_nodes.len() as u16 + frequencies.len() as u16 - 1);
    }

    lengths.fill(0);
    let mut stack = vec![(nodes.pop().unwrap().1, 0u32)];
    while let Some((node, depth)) = stack.pop() {
        let node = node as usize;
        if node < frequencies.len() {
            lengths[node] = depth.min(255) as u8;
        } else {
            let (left, right) = internal_nodes[node - frequencies.len()];
            stack.push((left, depth + 1));
            stack.push((right, depth + 1));
        }
    }

    let max_length = lengths.iter().copied().max().unwrap_or(0);
    if max_length > length_limit {
        let mut counts = [0u32; 16];
        for &length in lengths.iter() {
            counts[usize::from(length.min(length_limit))] += 1;
        }
        let mut total = 0u32;
        for (i, count) in counts.iter().enumerate().skip(1).take(usize::from(length_limit)) {
            total += count << (usize::from(length_limit) - i);
        }
        while total > 1u32 << length_limit {
            let mut i = usize::from(length_limit) - 1;
            while counts[i] == 0 {
                if i == 0 {
                    return Err("霍夫曼码长无法缩短".into());
                }
                i -= 1;
            }
            if counts[usize::from(length_limit)] == 0 {
                return Err("霍夫曼码长无法缩短".into());
            }
            counts[i] -= 1;
            counts[usize::from(length_limit)] -= 1;
            counts[i + 1] += 2;
            total -= 1;
        }
        let mut len = length_limit;
        let mut indexes = frequencies.iter().copied().enumerate().collect::<Vec<_>>();
        indexes.sort_unstable_by_key(|&(_, frequency)| frequency);
        for &(i, frequency) in &indexes {
            if frequency > 0 {
                while counts[usize::from(len)] == 0 {
                    if len == 0 {
                        return Err("霍夫曼码长分配失败".into());
                    }
                    len -= 1;
                }
                lengths[i] = len;
                counts[usize::from(len)] -= 1;
            }
        }
    }

    codes.fill(0);
    let mut code = 0u32;
    for len in 1..=length_limit {
        for (i, &length) in lengths.iter().enumerate() {
            if length == len {
                codes[i] = (code as u16).reverse_bits() >> (16 - len);
                code += 1;
            }
        }
        code <<= 1;
    }
    if code != 2u32 << length_limit {
        return Err("霍夫曼树不完整".into());
    }
    Ok(true)
}

fn write_huffman_tree(bits: &mut Bits, frequencies: &[u32], lengths: &mut [u8], codes: &mut [u16]) -> Result<(), String> {
    if !build_huffman_tree(frequencies, lengths, codes, 15)? {
        let symbol = frequencies.iter().position(|&frequency| frequency > 0).unwrap_or(0);
        return write_single_entry(bits, symbol as u16);
    }

    let mut code_length_lengths = [0u8; 16];
    let mut code_length_codes = [0u16; 16];
    let mut code_length_frequencies = [0u32; 16];
    for &length in lengths.iter() {
        code_length_frequencies[usize::from(length)] += 1;
    }
    let single = !build_huffman_tree(&code_length_frequencies, &mut code_length_lengths, &mut code_length_codes, 7)?;

    const ORDER: [usize; 19] = [17, 18, 0, 1, 2, 3, 4, 5, 16, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
    bits.write_bits(0, 1);
    bits.write_bits(19 - 4, 4);
    for i in ORDER {
        if i > 15 || code_length_frequencies[i] == 0 {
            bits.write_bits(0, 3);
        } else if single {
            bits.write_bits(1, 3);
        } else {
            bits.write_bits(u64::from(code_length_lengths[i]), 3);
        }
    }

    match lengths.len() {
        256 => {
            bits.write_bits(1, 1);
            bits.write_bits(3, 3);
            bits.write_bits(254, 8);
        }
        280 => bits.write_bits(0, 1),
        _ => return Err("霍夫曼字母表长度不对".into()),
    }

    if !single {
        for &len in lengths.iter() {
            let n = code_length_lengths[usize::from(len)];
            if n == 0 {
                return Err("码长表缺少符号".into());
            }
            bits.write_bits(u64::from(code_length_codes[usize::from(len)]), n);
        }
    }
    Ok(())
}
