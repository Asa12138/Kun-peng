// kraken 2 使用的是murmur_hash3 算法的 fmix64作为 hash
use crate::fmix64 as murmur_hash3;
use std::collections::VecDeque;

pub const DEFAULT_TOGGLE_MASK: u64 = 0xe37e28c4271b5a2d;
pub const DEFAULT_SPACED_SEED_MASK: u64 = 0;
pub const CURRENT_REVCOM_VERSION: u8 = 1;

/// minimizer config
#[derive(Copy, Debug, Clone)]
pub struct Meros {
    pub k_mer: usize,
    pub l_mer: usize,
    pub mask: u64,
    pub spaced_seed_mask: u64,
    pub toggle_mask: u64,
    pub min_clear_hash_value: Option<u64>,
}

impl Meros {
    pub fn new(
        k_mer: usize,
        l_mer: usize,
        spaced_seed_mask: Option<u64>,
        toggle_mask: Option<u64>,
        min_clear_hash_value: Option<u64>,
    ) -> Self {
        let mut mask = 1u64;
        mask <<= l_mer * BITS_PER_CHAR;
        mask -= 1;

        Self {
            k_mer,
            l_mer,
            mask,
            spaced_seed_mask: spaced_seed_mask.unwrap_or(DEFAULT_SPACED_SEED_MASK),
            toggle_mask: toggle_mask.unwrap_or(DEFAULT_TOGGLE_MASK) & mask,
            min_clear_hash_value,
        }
    }

    pub fn window_size(&self) -> usize {
        self.k_mer - self.l_mer
    }
}

#[cfg(feature = "dna")]
pub const BITS_PER_CHAR: usize = 2;
#[cfg(feature = "protein")]
pub const BITS_PER_CHAR: usize = 4;

#[inline]
fn reverse_complement(mut kmer: u64, n: usize) -> u64 {
    // Reverse bits while leaving bit pairs (nucleotides) intact.

    // Swap consecutive pairs of bits
    kmer = (kmer >> 2 & 0x3333333333333333) | (kmer << 2 & 0xCCCCCCCCCCCCCCCC);

    // Swap consecutive nibbles (4-bit groups)
    kmer = (kmer >> 4 & 0x0F0F0F0F0F0F0F0F) | (kmer << 4 & 0xF0F0F0F0F0F0F0F0);

    // Swap consecutive bytes
    kmer = (kmer >> 8 & 0x00FF00FF00FF00FF) | (kmer << 8 & 0xFF00FF00FF00FF00);

    // Swap consecutive pairs of bytes
    kmer = (kmer >> 16 & 0x0000FFFF0000FFFF) | (kmer << 16 & 0xFFFF0000FFFF0000);

    // Swap the two halves of the 64-bit word
    kmer = (kmer >> 32) | (kmer << 32);

    // Complement the bits, shift to the right length, and mask to get the desired length
    (!kmer >> (64 - n * 2)) & ((1u64 << (n * 2)) - 1)

    // if revcom_version == 0 {
    //     // Complement the bits and mask to get the desired length
    //     !kmer & ((1u64 << (n * 2)) - 1)
    // } else {
    //     // Complement the bits, shift to the right length, and mask to get the desired length
    //     (!kmer >> (64 - n * 2)) & ((1u64 << (n * 2)) - 1)
    // }
}

#[cfg(feature = "dna")]
#[inline]
fn canonical_representation(kmer: u64, n: usize) -> u64 {
    let revcom = reverse_complement(kmer, n);
    if kmer < revcom {
        kmer
    } else {
        revcom
    }
}

#[cfg(feature = "protein")]
#[inline]
fn canonical_representation(kmer: u64, n: usize, revcom_version: u8) -> u64 {
    kmer
}

#[cfg(feature = "dna")]
#[inline]
fn char_to_value(c: u8) -> Option<u64> {
    match c {
        b'A' | b'a' => Some(0x00),
        b'C' | b'c' => Some(0x01),
        b'G' | b'g' => Some(0x02),
        b'T' | b't' => Some(0x03),
        _ => None,
    }
}

#[cfg(feature = "protein")]
#[inline]
fn char_to_value(c: u8) -> Option<64> {
    match c {
        // stop codons/rare amino acids
        b'*' | b'U' | b'u' | b'O' | b'o' => Some(0x00),
        // alanine
        b'A' | b'a' => Some(0x01),
        // asparagine, glutamine, serine
        b'N' | b'n' | b'Q' | b'q' | b'S' | b's' => Some(0x02),
        // cysteine
        b'C' | b'c' => Some(0x03),
        // aspartic acid, glutamic acid
        b'D' | b'd' | b'E' | b'e' => Some(0x04),
        // phenylalanine
        b'F' | b'f' => Some(0x05),
        // glycine
        b'G' | b'g' => Some(0x06),
        // histidine
        b'H' | b'h' => Some(0x07),
        // isoleucine, leucine
        b'I' | b'i' | b'L' | b'l' => Some(0x08),
        // lysine
        b'K' | b'k' => Some(0x09),
        // proline
        b'P' | b'p' => Some(0x0a),
        // arginine
        b'R' | b'r' => Some(0x0b),
        // methionine, valine
        b'M' | b'm' | b'V' | b'v' => Some(0x0c),
        // threonine
        b'T' | b't' => Some(0x0d),
        // tryptophan
        b'W' | b'w' => Some(0x0e),
        // tyrosine
        b'Y' | b'y' => Some(0x0f),
        _ => None,
    }
}

#[derive(Debug)]
struct MinimizerData {
    pos: usize,
    candidate_lmer: u64,
}

impl MinimizerData {
    fn new(candidate_lmer: u64, pos: usize) -> Self {
        Self {
            candidate_lmer,
            pos,
        }
    }
}

pub struct MinimizerWindow {
    queue: VecDeque<MinimizerData>,
    /// 窗口队列的大小
    capacity: usize,
    /// 队列计数
    count: usize,
}

impl MinimizerWindow {
    fn new(capacity: usize) -> Self {
        Self {
            queue: VecDeque::with_capacity(capacity),
            capacity,
            count: 0,
        }
    }

    #[inline]
    fn next(&mut self, candidate_lmer: u64) -> Option<u64> {
        // 无需比较，直接返回
        if self.capacity == 1 {
            return Some(candidate_lmer);
        }

        let data = MinimizerData::new(candidate_lmer, self.count);

        // 移除队列中所有比当前元素大的元素的索引
        // 因为它们不可能是当前窗口的最小值
        while let Some(m_data) = self.queue.back() {
            if m_data.candidate_lmer > candidate_lmer {
                self.queue.pop_back();
            } else {
                break;
            }
        }
        // 将当前元素的索引添加到队列
        self.queue.push_back(data);
        // 确保队列的第一个元素在当前窗口内
        if self.count < self.capacity {
            self.count += 1;
            return None;
        } else if self
            .queue
            .front()
            .map_or(false, |front| front.pos < self.count - self.capacity)
        {
            self.queue.pop_front();
        }

        self.count += 1;
        self.queue.front().map(|front| front.candidate_lmer)
    }

    fn clear(&mut self) {
        self.count = 0;
        self.queue.clear();
    }
}

// pub struct MinimizerWindow {
//     queue: Vec<u64>,
//     /// 当前窗口计数
//     count: usize,
//     /// 窗口队列的大小
//     capacity: usize,
//     /// 当前最小值
//     cur_minimizer: Option<u64>,
// }

// impl MinimizerWindow {
//     fn new(k_mer: usize, l_mer: usize) -> Self {
//         let capacity: usize = k_mer - l_mer + 1;
//         Self {
//             queue: Vec::with_capacity(capacity),
//             capacity,
//             count: 0,
//             cur_minimizer: None,
//         }
//     }

//     fn set_minimizer(&mut self, item: Option<u64>) {
//         self.cur_minimizer = item;
//         self.queue.clear();
//     }

//     // 3, 4, 2, 5, 1 k=3 => 2, 1
//     // 1, 2, 3, 4  k=2 => 1, 2, 3
//     #[inline]
//     fn next_candidate_lmer(&mut self, item: u64) -> Option<u64> {
//         // 无需比较，直接返回
//         if self.capacity == 1 {
//             return Some(item);
//         }
//         match self.cur_minimizer {
//             Some(mizer) if mizer < item => self.queue.push(item),
//             _ => self.set_minimizer(Some(item)),
//         }

//         self.count += 1;
//         if self.count >= self.capacity {
//             self.count = 0;
//             let cur = self.cur_minimizer;
//             self.set_minimizer(self.queue.iter().min().copied());
//             cur
//         } else {
//             None
//         }
//     }

//     fn get_last_minimizer(&mut self) -> Option<u64> {
//         self.cur_minimizer
//             .or_else(|| self.queue.iter().min().copied())
//     }

//     fn clear(&mut self) {
//         self.cur_minimizer = None;
//         self.count = 0;
//         self.queue.clear();
//     }
// }

struct Cursor {
    pos: usize,
    end: usize,
    inner: Vec<u64>,
    capacity: usize,
    value: u64,
    mask: u64,
    window: MinimizerWindow,
}

impl Cursor {
    fn new(meros: &Meros) -> Self {
        Self {
            pos: 0,
            end: 0,
            inner: Vec::with_capacity(meros.l_mer),
            capacity: meros.l_mer,
            value: 0,
            mask: meros.mask,
            window: MinimizerWindow::new(meros.window_size()),
        }
    }

    // 每次取一个 lmer 值出来，如果为空，表示一直 seq 已处理完成
    #[inline]
    fn slide(&mut self, seq: &[u8]) -> Option<u64> {
        while self.pos < self.end {
            let ch = seq[self.pos];
            let code = if ch == b'\n' || ch == b'\r' {
                self.pos += 1;
                char_to_value(seq[self.pos])
            } else {
                char_to_value(ch)
            };
            // let code = char_to_value(seq[self.pos]);
            self.pos += 1;
            if let Some(c) = code {
                if let Some(lmer) = self.next_lmer(c) {
                    return Some(lmer);
                }
            } else {
                self.clear();
            }
        }
        None
    }

    fn next_lmer(&mut self, item: u64) -> Option<u64> {
        self.value <<= BITS_PER_CHAR;
        self.value |= item;
        if self.inner.len() == self.capacity {
            self.inner.remove(0); // 移除最旧的元素
        }
        self.inner.push(item); // 使用 push 方法
        if self.inner.len() >= self.capacity {
            self.value &= self.mask;
            return Some(self.value);
        }

        None
    }

    #[inline]
    fn next_candidate_lmer(&mut self, item: u64) -> Option<u64> {
        self.window.next(item)
    }

    pub fn has_next(&self) -> bool {
        return self.pos < self.end;
    }

    // 清除元素
    #[inline]
    fn clear(&mut self) {
        self.inner.clear();
        self.value = 0;
        self.window.clear();
    }
}

pub struct MinimizerScanner {
    meros: Meros,
    // l_mer: usize,
    cursor: Cursor,
    /// 存最近一个最小值
    last_minimizer: u64,
    // spaced_seed_mask: u64,
    // toggle_mask: u64,
}

impl MinimizerScanner {
    pub fn reset(&mut self) {
        self.cursor.clear();
        self.last_minimizer = std::u64::MAX;
    }

    pub fn new(meros: Meros) -> Self {
        Self {
            meros,
            cursor: Cursor::new(&meros),
            last_minimizer: std::u64::MAX,
        }
    }

    pub fn set_seq_end(&mut self, seq: &[u8]) {
        self.cursor.pos = 0;
        self.cursor.end = seq.len();
    }

    #[inline]
    fn to_candidate_lmer(&self, lmer: u64) -> u64 {
        let mut canonical_lmer = canonical_representation(lmer, self.meros.l_mer);
        if self.meros.spaced_seed_mask > 0 {
            canonical_lmer &= self.meros.spaced_seed_mask;
        }
        canonical_lmer ^ self.meros.toggle_mask
    }

    fn get_last_minimizer(&mut self) -> Option<u64> {
        None
        // self.cursor
        //     .window
        //     .get_last_minimizer()
        //     .map(|minimizer| minimizer ^ self.toggle_mask)
    }

    /// 在一个序列上滑动一个光标（可能是为了找到下一个有意义的片段或窗口），
    /// 并对滑动得到的片段进行某种转换或处理。如果光标无法继续滑动（例如到达序列的末尾），则返回 None。
    fn next_window(&mut self, seq: &[u8]) -> Option<u64> {
        self.cursor.slide(seq).and_then(|lmer| {
            let candidate_lmer = self.to_candidate_lmer(lmer);
            self.cursor.next_candidate_lmer(candidate_lmer)
        })
    }

    /// 这个函数通过 next_minimizer 获取序列的下一个最小化器，然后使用 murmur_hash3 对其进行哈希处理，
    /// 最后根据设定的最小哈希值进行筛选
    pub fn next_hashed_minimizer(&mut self, seq: &[u8]) -> Option<u64> {
        while let Some(minimizer) = self.next_minimizer_inclusive(seq) {
            let hashed = murmur_hash3(minimizer);
            if self
                .meros
                .min_clear_hash_value
                .map_or(true, |min_hash| hashed >= min_hash)
            {
                return Some(hashed);
            }
            // 如果哈希值不满足条件，循环将继续尝试下一个最小化器
        }
        None // 当没有更多的最小化器时返回 None
    }

    pub fn next_minimizer_inclusive(&mut self, seq: &[u8]) -> Option<u64> {
        while self.cursor.has_next() {
            if let Some(minimizer) = self.next_window(&seq) {
                // 直接返回当前最小化器，不进行重复检查
                return Some(minimizer ^ self.meros.toggle_mask);
            }
        }
        // 如果循环结束还没有找到最小化器，检查是否有遗留的最小化器
        let last_minimizer = self.get_last_minimizer();
        // 清空所有的值，准备下一次获取
        self.cursor.clear();
        last_minimizer
    }

    /// 去除重复的值
    pub fn next_minimizer(&mut self, seq: &[u8]) -> Option<u64> {
        while self.cursor.has_next() {
            if let Some(minimizer) = self.next_window(&seq) {
                if minimizer != self.last_minimizer {
                    self.last_minimizer = minimizer;
                    return Some(minimizer ^ self.meros.toggle_mask);
                }
            }
            // if let Some(lmer) = self.cursor.slide(&seq) {
            //     let candidate_lmer = self.to_candidate_lmer(lmer);
            //     if let Some(minimizer) = self.cursor.next_candidate_lmer(candidate_lmer) {
            //         if minimizer != self.last_minimizer {
            //             self.last_minimizer = minimizer;
            //             return Some(minimizer ^ self.toggle_mask);
            //         }
            //     }
            // }
        }
        // 检查滑动队列中是否存在值
        let last_minimizer = self.get_last_minimizer();
        // 清空所有的值，等下次换取时，必然等于 None
        self.cursor.clear();
        last_minimizer
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // 编写测试函数
    #[test]
    fn test_minimizer_scanner() {
        // 在这里编写测试代码
        // 使用 assert_eq!、assert!、assert_ne! 等宏来断言测试条件是否为真
        // 如果条件不为真，测试将失败
        let seq: Vec<u8> = b"ACGATCGACGACG".to_vec();
        let meros = Meros::new(10, 5, None, None, None);
        let mut scanner = MinimizerScanner::new(meros);
        scanner.set_seq_end(&seq);
        let m1 = scanner.next_minimizer(&seq);
        let mm1 = format!("{:016x}", m1.unwrap());
        assert_eq!(mm1, "00000000000002d8");
        let m2 = scanner.next_minimizer(&seq);
        let mm2 = format!("{:016x}", m2.unwrap());
        assert_eq!(mm2, "0000000000000218");
    }

    #[test]
    fn test_minimizer() {
        // 1, 2, 3, 4
        let seq: Vec<u64> = vec![1, 2, 3, 4];
        // 窗口大小 = 2 - 0 + 1
        let mut mini: MinimizerWindow = MinimizerWindow::new(1);
        let mut result = vec![];
        for s in seq {
            if let Some(a) = mini.next(s) {
                result.push(a);
            }
        }
        // if let Some(a) = mini.get_last_minimizer() {
        //     result.push(a);
        // }
        assert_eq!(result, [1, 2, 3, 4]);

        let seq: Vec<u64> = vec![4, 3, 5, 2, 6, 2, 1];
        // 窗口大小 = 2 - 0 + 1
        let mut mini = MinimizerWindow::new(2);
        let mut result = vec![];
        for s in seq {
            if let Some(a) = mini.next(s) {
                result.push(a);
            }
        }
        // if let Some(a) = mini.get_last_minimizer() {
        //     result.push(a);
        // }
        assert_eq!(result, [3, 2, 2, 2, 1]);
    }
}
