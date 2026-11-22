//! Read-only access to the filesystem inside a console save partition image.

use crate::Error;

pub const MAGIC: &[u8; 4] = b"SAVE";
const DIR_ENTRY: usize = 0x28;
const FILE_ENTRY: usize = 0x30;
const NAME_LEN: usize = 0x10;
/// Deepest directory nesting followed when building paths.
const MAX_DEPTH: usize = 32;

fn bad(m: String) -> Error {
    Error::Format(format!("save image: {m}"))
}

fn u32_at(b: &[u8], at: usize) -> Result<u32, Error> {
    b.get(at..at + 4)
        .map(|s| u32::from_le_bytes(s.try_into().unwrap()))
        .ok_or_else(|| bad(format!("read past the end at {at:#x}")))
}

fn u64_at(b: &[u8], at: usize) -> Result<u64, Error> {
    b.get(at..at + 8)
        .map(|s| u64::from_le_bytes(s.try_into().unwrap()))
        .ok_or_else(|| bad(format!("read past the end at {at:#x}")))
}

fn name_at(b: &[u8], at: usize) -> String {
    let raw = &b[at..at + NAME_LEN];
    let end = raw.iter().position(|&c| c == 0).unwrap_or(NAME_LEN);
    raw[..end].iter().map(|&c| c as char).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct File {
    /// Slash-separated path from the root.
    pub path: String,
    pub name: String,
    pub size: u64,
    first_block: u32,
}

pub struct SaveFs<'a> {
    data: &'a [u8],
    block_size: usize,
    fat: usize,
    fat_count: usize,
    data_start: usize,
    data_blocks: usize,
    dirs: usize,
    max_dirs: usize,
    files: usize,
    max_files: usize,
}

impl<'a> SaveFs<'a> {
    pub fn open(data: &'a [u8]) -> Result<Self, Error> {
        if data.get(..4) != Some(&MAGIC[..]) {
            return Err(bad("no SAVE magic".into()));
        }
        let info = u64_at(data, 8)? as usize;
        let block_size = u32_at(data, info + 0x04)? as usize;
        if block_size == 0 || !block_size.is_power_of_two() || block_size > 1 << 20 {
            return Err(bad(format!("block size {block_size:#x}")));
        }
        let data_start = u64_at(data, info + 0x38)? as usize;
        let at_block = |off: usize| -> Result<usize, Error> {
            Ok(data_start + u32_at(data, info + off)? as usize * block_size)
        };
        let fs = SaveFs {
            data,
            block_size,
            fat: u64_at(data, info + 0x28)? as usize,
            fat_count: u32_at(data, info + 0x30)? as usize,
            data_start,
            data_blocks: u32_at(data, info + 0x40)? as usize,
            dirs: at_block(0x48)?,
            max_dirs: u32_at(data, info + 0x50)? as usize,
            files: at_block(0x58)?,
            max_files: u32_at(data, info + 0x60)? as usize,
        };
        if fs.data_blocks > fs.fat_count {
            return Err(bad(format!(
                "{} data blocks but {} allocation entries",
                fs.data_blocks, fs.fat_count
            )));
        }
        for (what, at, len) in [
            ("allocation table", fs.fat, (fs.fat_count + 1) * 8),
            ("data region", fs.data_start, fs.data_blocks * block_size),
            ("directory table", fs.dirs, fs.max_dirs * DIR_ENTRY),
            ("file table", fs.files, fs.max_files * FILE_ENTRY),
        ] {
            if at.checked_add(len).is_none_or(|end| end > data.len()) {
                return Err(bad(format!("{what} runs past the image")));
            }
        }
        Ok(fs)
    }

    fn node(&self, n: usize) -> (u32, u32) {
        let at = self.fat + n * 8;
        (
            u32::from_le_bytes(self.data[at..at + 4].try_into().unwrap()),
            u32::from_le_bytes(self.data[at + 4..at + 8].try_into().unwrap()),
        )
    }

    /// The data-block ranges a file occupies, refusing chains that loop, overlap or leave the data region.
    fn runs(&self, first_block: u32) -> Result<Vec<(usize, usize)>, Error> {
        let mut seen = vec![false; self.data_blocks + 2];
        let mut out = Vec::new();
        let mut node = first_block as usize + 1;
        let mut count = 0;
        while node != 0 {
            if !(1..=self.data_blocks).contains(&node) {
                return Err(bad(format!("allocation chain reaches node {node}")));
            }
            let (_, next) = self.node(node);
            let mut end = node;
            if next & 0x8000_0000 != 0 {
                end = (self.node(node + 1).1 & 0x7fff_ffff) as usize;
                if !(node..=self.data_blocks).contains(&end) {
                    return Err(bad(format!("run {node}..{end}")));
                }
            }
            count += end - node + 1;
            if count > self.data_blocks || seen[node..=end].iter().any(|&s| s) {
                return Err(bad("allocation chain loops".into()));
            }
            seen[node..=end].fill(true);
            out.push((node - 1, end));
            node = (next & 0x7fff_ffff) as usize;
        }
        Ok(out)
    }

    pub fn read(&self, f: &File, limit: usize) -> Result<Vec<u8>, Error> {
        let size = f.size as usize;
        if size > limit || size > self.data_blocks * self.block_size {
            return Err(bad(format!("{} is too large ({size} bytes)", f.path)));
        }
        let mut out = Vec::with_capacity(size);
        for (first, end) in self.runs(f.first_block)? {
            if out.len() >= size {
                break;
            }
            let from = self.data_start + first * self.block_size;
            let to = self.data_start + end * self.block_size;
            out.extend_from_slice(&self.data[from..to]);
        }
        if out.len() < size {
            return Err(bad(format!("{} is cut short", f.path)));
        }
        out.truncate(size);
        Ok(out)
    }

    fn dir_path(&self, mut i: usize) -> String {
        let mut parts = Vec::new();
        for _ in 0..MAX_DEPTH {
            if i == 0 || i >= self.max_dirs {
                break;
            }
            let at = self.dirs + i * DIR_ENTRY;
            let name = name_at(self.data, at + 4);
            if name.is_empty() {
                break;
            }
            parts.push(name);
            let parent = u32::from_le_bytes(self.data[at..at + 4].try_into().unwrap()) as usize;
            if parent == i {
                break;
            }
            i = parent;
        }
        parts.reverse();
        parts.join("/")
    }

    /// Every file with a name and contents.
    pub fn files(&self) -> Vec<File> {
        (1..self.max_files)
            .filter_map(|i| {
                let at = self.files + i * FILE_ENTRY;
                let name = name_at(self.data, at + 4);
                let size = u64::from_le_bytes(self.data[at + 0x20..at + 0x28].try_into().unwrap());
                if name.is_empty() || size == 0 || size > u32::MAX as u64 {
                    return None;
                }
                let parent = u32::from_le_bytes(self.data[at..at + 4].try_into().unwrap());
                let dir = self.dir_path(parent as usize);
                Some(File {
                    path: if dir.is_empty() {
                        name.clone()
                    } else {
                        format!("{dir}/{name}")
                    },
                    name,
                    size,
                    first_block: u32::from_le_bytes(
                        self.data[at + 0x1c..at + 0x20].try_into().unwrap(),
                    ),
                })
            })
            .collect()
    }
}

/// Builds a small image for tests: `dirs` as (parent, name), `files` as (parent, name, contents).
#[cfg(test)]
pub fn build(dirs: &[(u32, &str)], files: &[(u32, &str, &[u8])]) -> Vec<u8> {
    const BS: usize = 0x200;
    let info = 0x20;
    let blocks_for = |n: usize| n.div_ceil(BS).max(1);
    let table_blocks =
        blocks_for((dirs.len() + 2) * DIR_ENTRY) + blocks_for((files.len() + 2) * FILE_ENTRY);
    let file_blocks: usize = files.iter().map(|f| blocks_for(f.2.len())).sum();
    let data_blocks = table_blocks + file_blocks;
    let fat = 0x200;
    let data_start = (fat + (data_blocks + 1) * 8).next_multiple_of(BS);
    let mut img = vec![0u8; data_start + data_blocks * BS];
    img[..4].copy_from_slice(MAGIC);
    img[8..16].copy_from_slice(&(info as u64).to_le_bytes());
    let put32 =
        |img: &mut Vec<u8>, at: usize, v: u32| img[at..at + 4].copy_from_slice(&v.to_le_bytes());
    let put64 =
        |img: &mut Vec<u8>, at: usize, v: u64| img[at..at + 8].copy_from_slice(&v.to_le_bytes());
    put32(&mut img, info + 4, BS as u32);
    put64(&mut img, info + 0x28, fat as u64);
    put32(&mut img, info + 0x30, data_blocks as u32);
    put64(&mut img, info + 0x38, data_start as u64);
    put32(&mut img, info + 0x40, data_blocks as u32);
    let dir_block = 0;
    let file_block = blocks_for((dirs.len() + 2) * DIR_ENTRY);
    put32(&mut img, info + 0x48, dir_block as u32);
    put32(&mut img, info + 0x50, (dirs.len() + 2) as u32);
    put32(&mut img, info + 0x58, file_block as u32);
    put32(&mut img, info + 0x60, (files.len() + 2) as u32);
    for (i, (parent, name)) in dirs.iter().enumerate() {
        let at = data_start + (i + 1) * DIR_ENTRY;
        put32(&mut img, at, *parent);
        img[at + 4..at + 4 + name.len()].copy_from_slice(name.as_bytes());
    }
    let mut next = table_blocks;
    for (i, (parent, name, data)) in files.iter().enumerate() {
        let at = data_start + file_block * BS + (i + 1) * FILE_ENTRY;
        put32(&mut img, at, *parent);
        img[at + 4..at + 4 + name.len()].copy_from_slice(name.as_bytes());
        put32(&mut img, at + 0x1c, next as u32);
        put64(&mut img, at + 0x20, data.len() as u64);
        let n = blocks_for(data.len());
        let node = next + 1;
        if n == 1 {
            put32(&mut img, fat + node * 8 + 4, 0);
        } else {
            put32(&mut img, fat + node * 8 + 4, 0x8000_0000);
            put32(
                &mut img,
                fat + (node + 1) * 8 + 4,
                (node + n - 1) as u32 | 0x8000_0000,
            );
        }
        let from = data_start + next * BS;
        img[from..from + data.len()].copy_from_slice(data);
        next += n;
    }
    img
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_and_reads_files() {
        let big: Vec<u8> = (0..1500u32).map(|i| i as u8).collect();
        let img = build(
            &[(0, "CEC"), (1, "00020800")],
            &[(1, "MBoxList____", b"list"), (2, "_abc", &big)],
        );
        let fs = SaveFs::open(&img).unwrap();
        let files = fs.files();
        let paths: Vec<&str> = files.iter().map(|f| f.path.as_str()).collect();
        assert_eq!(paths, ["CEC/MBoxList____", "CEC/00020800/_abc"]);
        assert_eq!(fs.read(&files[0], 100).unwrap(), b"list");
        assert_eq!(fs.read(&files[1], 4096).unwrap(), big);
        assert!(fs.read(&files[1], 100).is_err());
    }

    #[test]
    fn refuses_damage() {
        assert!(SaveFs::open(b"DISA").is_err());
        let mut img = build(&[(0, "CEC")], &[(1, "a", b"x")]);
        img.truncate(0x300);
        assert!(SaveFs::open(&img).is_err());
        let mut img = build(&[(0, "CEC")], &[(1, "a", b"x")]);
        let fs = SaveFs::open(&img).unwrap();
        let fat = 0x200;
        let file = fs.files()[0].clone();
        let node = file.first_block as usize + 1;
        // A chain that points back at itself.
        img[fat + node * 8 + 4..fat + node * 8 + 8].copy_from_slice(&(node as u32).to_le_bytes());
        assert!(SaveFs::open(&img).unwrap().read(&file, 100).is_err());
    }
}
