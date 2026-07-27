//! Just enough .NET metadata to identify a Decal plugin in a DLL.
//!
//! Registering a managed plugin normally means running `RegAsm`, which needs a
//! .NET Framework install inside the prefix. We would rather not require one just
//! to add a plugin, and we do not have to: `RegAsm` only writes registry keys, and
//! everything it needs is already in the assembly. So we read it ourselves.
//!
//! Three facts come out of a plugin DLL:
//!
//!   * the **CLSID** — the `[Guid("…")]` on the plugin class, which is the key name
//!     Decal files the plugin under;
//!   * the **type name** — `Namespace.Class`, which the Decal adapter surrogate
//!     instantiates;
//!   * the **friendly name** — `[FriendlyName("…")]`, the label Decal (and now our
//!     settings list) shows.
//!
//! Scope is deliberately narrow. Reaching `CustomAttribute` (table 0x0C) means
//! being able to size every table before it, so tables 0x00–0x0C are modelled and
//! nothing after them is: their row *counts* are read (coded-index widths need
//! them) but their layouts are never needed. That keeps this a couple of hundred
//! lines instead of the whole ECMA-335 schema.

use std::path::Path;

/// What a plugin DLL says about itself.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PluginIdentity {
    /// Braced, upper-case, as the registry wants it.
    pub clsid: String,
    /// `Namespace.Class`.
    pub object: String,
    /// `[FriendlyName]` if it has one, else the class name.
    pub name: String,
}

/// Identify the Decal plugin class in `dll`.
///
/// Errors rather than guesses: a DLL with no `[Guid]`-carrying type is not
/// something we can register, and saying so beats writing a broken key.
pub fn plugin_identity(dll: &Path) -> Result<PluginIdentity, String> {
    let bytes = std::fs::read(dll).map_err(|e| format!("{}: {e}", dll.display()))?;
    identity_from_bytes(&bytes).map_err(|e| format!("{}: {e}", dll.display()))
}

// ------------------------------------------------------------------ PE + streams

fn u16le(b: &[u8], at: usize) -> u32 {
    u16::from_le_bytes([b[at], b[at + 1]]) as u32
}
fn u32le(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// Map a virtual address to a file offset using the section table.
fn rva_to_offset(b: &[u8], pe: usize, rva: u32) -> Option<usize> {
    let nsec = u16le(b, pe + 6) as usize;
    let optsz = u16le(b, pe + 20) as usize;
    let sections = pe + 24 + optsz;
    for i in 0..nsec {
        let s = sections + 40 * i;
        if s + 40 > b.len() {
            return None;
        }
        let va = u32le(b, s + 12);
        let vsize = u32le(b, s + 8).max(u32le(b, s + 16));
        let raw = u32le(b, s + 20);
        if rva >= va && rva < va + vsize {
            return Some((raw + (rva - va)) as usize);
        }
    }
    None
}

struct Metadata<'a> {
    strings: &'a [u8],
    blobs: &'a [u8],
    /// Table id -> (row count, file offset of its first row, row size).
    tables: Vec<(u32, usize, usize)>,
    heap_sizes: u8,
}

impl<'a> Metadata<'a> {
    fn parse(b: &'a [u8]) -> Result<Metadata<'a>, String> {
        if b.len() < 0x40 || &b[0..2] != b"MZ" {
            return Err("not a PE file".into());
        }
        let pe = u32le(b, 0x3c) as usize;
        if pe + 24 > b.len() || &b[pe..pe + 4] != b"PE\0\0" {
            return Err("not a PE file".into());
        }
        let opt = pe + 24;
        let magic = u16le(b, opt);
        // Data directory 14 is the CLR header; PE32 and PE32+ put it at different
        // offsets because of the extra 64-bit fields in between.
        let dd = opt + if magic == 0x10b { 96 } else { 112 };
        let clr_rva = u32le(b, dd + 14 * 8);
        if clr_rva == 0 {
            return Err("not a .NET assembly (no CLR header)".into());
        }
        let clr = rva_to_offset(b, pe, clr_rva).ok_or("bad CLR header address")?;
        let md_rva = u32le(b, clr + 8);
        let root = rva_to_offset(b, pe, md_rva).ok_or("bad metadata address")?;
        if root + 20 > b.len() || &b[root..root + 4] != b"BSJB" {
            return Err("metadata signature missing".into());
        }

        // Root: signature, versions, reserved, version string length, then flags
        // and the stream count.
        let vlen = u32le(b, root + 12) as usize;
        let mut p = root + 16 + vlen.div_ceil(4) * 4;
        let nstreams = u16le(b, p + 2) as usize;
        p += 4;

        let (mut strings, mut blobs, mut tables_at) = (None, None, None);
        for _ in 0..nstreams {
            let off = u32le(b, p) as usize;
            let size = u32le(b, p + 4) as usize;
            let name_at = p + 8;
            let end = b[name_at..].iter().position(|&c| c == 0).ok_or("bad stream name")?;
            let name = &b[name_at..name_at + end];
            let start = root + off;
            match name {
                b"#Strings" => strings = Some(&b[start..(start + size).min(b.len())]),
                b"#Blob" => blobs = Some(&b[start..(start + size).min(b.len())]),
                // "#~" is the normal compressed form; "#-" is the uncompressed one
                // some obfuscators emit. The header we read is the same.
                b"#~" | b"#-" => tables_at = Some(start),
                _ => {}
            }
            p = name_at + (end + 1).div_ceil(4) * 4;
        }
        let tables_at = tables_at.ok_or("no metadata tables")?;
        let strings = strings.ok_or("no string heap")?;
        let blobs = blobs.ok_or("no blob heap")?;

        let heap_sizes = b[tables_at + 6];
        let valid = u64::from_le_bytes(b[tables_at + 8..tables_at + 16].try_into().unwrap());
        let mut q = tables_at + 24;
        let mut counts = [0u32; 64];
        for (i, count) in counts.iter_mut().enumerate() {
            if valid >> i & 1 == 1 {
                *count = u32le(b, q);
                q += 4;
            }
        }

        // Row layouts, in order, for every table up to CustomAttribute. Anything
        // beyond it is never addressed, so its layout is not modelled.
        let mut md =
            Metadata { strings, blobs, tables: vec![(0, 0, 0); 64], heap_sizes };
        for (i, count) in counts.iter().enumerate() {
            md.tables[i].0 = *count;
        }
        let mut at = q;
        for id in 0..=0x0Cu8 {
            let rows = counts[id as usize];
            let size = md.row_size(id, &counts)?;
            md.tables[id as usize] = (rows, at, size);
            at += rows as usize * size;
        }
        Ok(md)
    }

    fn str_idx(&self) -> usize {
        if self.heap_sizes & 1 != 0 {
            4
        } else {
            2
        }
    }
    fn guid_idx(&self) -> usize {
        if self.heap_sizes & 2 != 0 {
            4
        } else {
            2
        }
    }
    fn blob_idx(&self) -> usize {
        if self.heap_sizes & 4 != 0 {
            4
        } else {
            2
        }
    }

    /// Width of a simple index into one table: 2 bytes until it needs 4.
    fn simple(counts: &[u32; 64], table: u8) -> usize {
        if counts[table as usize] < (1 << 16) {
            2
        } else {
            4
        }
    }

    /// Width of a coded index: `bits` tag bits, so each target table may only use
    /// the remaining 16 - bits before the index widens to 4 bytes.
    fn coded(counts: &[u32; 64], tables: &[u8], bits: u32) -> usize {
        let max = tables.iter().map(|&t| counts[t as usize]).max().unwrap_or(0);
        if (max as u64) < (1u64 << (16 - bits)) {
            2
        } else {
            4
        }
    }

    fn row_size(&self, id: u8, counts: &[u32; 64]) -> Result<usize, String> {
        let s = self.str_idx();
        let g = self.guid_idx();
        let bl = self.blob_idx();
        const TYPE_DEF_OR_REF: &[u8] = &[0x02, 0x01, 0x1B];
        const HAS_CONSTANT: &[u8] = &[0x04, 0x08, 0x17];
        const HAS_CUSTOM_ATTR: &[u8] = &[
            0x06, 0x04, 0x01, 0x02, 0x08, 0x09, 0x0A, 0x00, 0x1A, 0x1B, 0x14, 0x17, 0x15, 0x02,
            0x23, 0x26, 0x27, 0x28, 0x00, 0x1D, 0x2A, 0x2C,
        ];
        const CUSTOM_ATTR_TYPE: &[u8] = &[0x06, 0x0A];
        const MEMBER_REF_PARENT: &[u8] = &[0x02, 0x01, 0x1A, 0x06, 0x1B];
        const RESOLUTION_SCOPE: &[u8] = &[0x00, 0x1A, 0x23, 0x01];
        Ok(match id {
            0x00 => 2 + s + 3 * g,                                        // Module
            0x01 => Self::coded(counts, RESOLUTION_SCOPE, 2) + 2 * s,      // TypeRef
            0x02 => {
                4 + 2 * s
                    + Self::coded(counts, TYPE_DEF_OR_REF, 2)
                    + Self::simple(counts, 0x04)
                    + Self::simple(counts, 0x06)
            } // TypeDef
            0x03 => Self::simple(counts, 0x04),                            // FieldPtr
            0x04 => 2 + s + bl,                                            // Field
            0x05 => Self::simple(counts, 0x06),                            // MethodPtr
            0x06 => 4 + 2 + 2 + s + bl + Self::simple(counts, 0x08),       // MethodDef
            0x07 => Self::simple(counts, 0x08),                            // ParamPtr
            0x08 => 2 + 2 + s,                                             // Param
            0x09 => Self::simple(counts, 0x02) + Self::coded(counts, TYPE_DEF_OR_REF, 2),
            0x0A => Self::coded(counts, MEMBER_REF_PARENT, 3) + s + bl,    // MemberRef
            0x0B => 2 + Self::coded(counts, HAS_CONSTANT, 2) + bl,         // Constant
            0x0C => {
                Self::coded(counts, HAS_CUSTOM_ATTR, 5)
                    + Self::coded(counts, CUSTOM_ATTR_TYPE, 3)
                    + bl
            } // CustomAttribute
            _ => return Err(format!("table 0x{id:02x} is beyond what this reader models")),
        })
    }

    fn rows(&self, id: u8) -> u32 {
        self.tables[id as usize].0
    }

    /// Read a `width`-byte little-endian field from row `row` (1-based) of a table.
    fn field(&self, b: &[u8], id: u8, row: u32, offset: usize, width: usize) -> u32 {
        let (_, at, size) = self.tables[id as usize];
        let p = at + (row as usize - 1) * size + offset;
        match width {
            2 => u16le(b, p),
            _ => u32le(b, p),
        }
    }

    fn string(&self, index: u32) -> String {
        let start = index as usize;
        if start >= self.strings.len() {
            return String::new();
        }
        let end = self.strings[start..].iter().position(|&c| c == 0).unwrap_or(0);
        String::from_utf8_lossy(&self.strings[start..start + end]).into_owned()
    }

    /// A blob's bytes, past its compressed length prefix.
    fn blob(&self, index: u32) -> &[u8] {
        let mut p = index as usize;
        if p >= self.blobs.len() {
            return &[];
        }
        let first = self.blobs[p];
        let len = if first & 0x80 == 0 {
            p += 1;
            (first & 0x7f) as usize
        } else if first & 0x40 == 0 {
            let n = (((first & 0x3f) as usize) << 8) | self.blobs[p + 1] as usize;
            p += 2;
            n
        } else {
            let n = (((first & 0x1f) as usize) << 24)
                | ((self.blobs[p + 1] as usize) << 16)
                | ((self.blobs[p + 2] as usize) << 8)
                | self.blobs[p + 3] as usize;
            p += 4;
            n
        };
        &self.blobs[p..(p + len).min(self.blobs.len())]
    }
}

/// A custom attribute's single string argument.
///
/// The value blob is a 2-byte prolog (0x0001) then a SerString: a compressed
/// length followed by UTF-8. Both `[Guid]` and `[FriendlyName]` have exactly this
/// shape, which is why one reader serves both.
fn single_string_arg(blob: &[u8]) -> Option<String> {
    if blob.len() < 3 || blob[0] != 0x01 || blob[1] != 0x00 {
        return None;
    }
    let b = &blob[2..];
    let (len, at) = match b[0] {
        0xFF => return None, // null string
        n if n & 0x80 == 0 => (n as usize, 1),
        n if n & 0x40 == 0 => ((((n & 0x3f) as usize) << 8) | b[1] as usize, 2),
        n => (
            (((n & 0x1f) as usize) << 24)
                | ((b[1] as usize) << 16)
                | ((b[2] as usize) << 8)
                | b[3] as usize,
            4,
        ),
    };
    (at + len <= b.len()).then(|| String::from_utf8_lossy(&b[at..at + len]).into_owned())
}

/// Walk `CustomAttribute` looking for `[Guid]` and `[FriendlyName]` on a type.
fn identify(b: &[u8], md: &Metadata) -> Result<PluginIdentity, String> {
    let s = md.str_idx();
    let bl = md.blob_idx();
    let counts: [u32; 64] = std::array::from_fn(|i| md.tables[i].0);

    const TYPE_DEF_OR_REF: &[u8] = &[0x02, 0x01, 0x1B];
    const HAS_CUSTOM_ATTR: &[u8] = &[
        0x06, 0x04, 0x01, 0x02, 0x08, 0x09, 0x0A, 0x00, 0x1A, 0x1B, 0x14, 0x17, 0x15, 0x02, 0x23,
        0x26, 0x27, 0x28, 0x00, 0x1D, 0x2A, 0x2C,
    ];
    const CUSTOM_ATTR_TYPE: &[u8] = &[0x06, 0x0A];
    const MEMBER_REF_PARENT: &[u8] = &[0x02, 0x01, 0x1A, 0x06, 0x1B];
    const RESOLUTION_SCOPE: &[u8] = &[0x00, 0x1A, 0x23, 0x01];

    let parent_w = Metadata::coded(&counts, HAS_CUSTOM_ATTR, 5);
    let type_w = Metadata::coded(&counts, CUSTOM_ATTR_TYPE, 3);
    let mrp_w = Metadata::coded(&counts, MEMBER_REF_PARENT, 3);
    let rs_w = Metadata::coded(&counts, RESOLUTION_SCOPE, 2);
    let tdor_w = Metadata::coded(&counts, TYPE_DEF_OR_REF, 2);

    // The attribute's ctor: a MemberRef whose Class is a TypeRef we can name.
    let attribute_name = |type_idx: u32| -> String {
        let (tag, row) = (type_idx & 0x7, type_idx >> 3);
        if tag != 3 || row == 0 || row > md.rows(0x0A) {
            return String::new(); // only MemberRef ctors are interesting here
        }
        let class = md.field(b, 0x0A, row, 0, mrp_w);
        let (ctag, crow) = (class & 0x7, class >> 3);
        if ctag != 1 || crow == 0 || crow > md.rows(0x01) {
            return String::new(); // TypeRef only
        }
        let name = md.string(md.field(b, 0x01, crow, rs_w, s));
        let ns = md.string(md.field(b, 0x01, crow, rs_w + s, s));
        if ns.is_empty() {
            name
        } else {
            format!("{ns}.{name}")
        }
    };

    let mut guid: Option<(u32, String)> = None; // (TypeDef row, guid)
    let mut friendly: Option<(u32, String)> = None;

    for row in 1..=md.rows(0x0C) {
        let parent = md.field(b, 0x0C, row, 0, parent_w);
        let ctor = md.field(b, 0x0C, row, parent_w, type_w);
        let value = md.field(b, 0x0C, row, parent_w + type_w, bl);
        // HasCustomAttribute tag 3 is TypeDef; anything else is not a class.
        if parent & 0x1f != 3 {
            continue;
        }
        let type_row = parent >> 5;
        let Some(arg) = single_string_arg(md.blob(value)) else { continue };
        match attribute_name(ctor).as_str() {
            "System.Runtime.InteropServices.GuidAttribute" => {
                guid.get_or_insert((type_row, arg));
            }
            n if n.ends_with("FriendlyNameAttribute") => {
                friendly.get_or_insert((type_row, arg));
            }
            _ => {}
        }
    }

    // Prefer the type that carries FriendlyName -- that is Decal's plugin class --
    // and fall back to whichever type has a Guid.
    let (type_row, clsid) = guid.ok_or(
        "no [Guid] attribute in this assembly, so there is no CLSID to register it under",
    )?;
    let type_row = friendly.as_ref().map_or(type_row, |(r, _)| *r);
    let clsid = format!("{{{}}}", clsid.trim_matches(['{', '}']).to_uppercase());

    if type_row == 0 || type_row > md.rows(0x02) {
        return Err("the [Guid] attribute is not on a type".into());
    }
    let name = md.string(md.field(b, 0x02, type_row, 4, s));
    let ns = md.string(md.field(b, 0x02, type_row, 4 + s, s));
    let object = if ns.is_empty() { name.clone() } else { format!("{ns}.{name}") };
    let _ = tdor_w; // TypeDef's Extends is skipped; kept for row-layout clarity.

    Ok(PluginIdentity {
        clsid,
        name: friendly.map(|(_, n)| n).unwrap_or_else(|| name.clone()),
        object,
    })
}

/// Public entry point, replacing the placeholder method above.
pub fn identity_from_bytes(bytes: &[u8]) -> Result<PluginIdentity, String> {
    let md = Metadata::parse(bytes)?;
    identify(bytes, &md)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_non_dotnet_file_is_rejected_clearly() {
        assert!(identity_from_bytes(b"not a pe at all").unwrap_err().contains("not a PE"));
    }

    #[test]
    fn a_native_pe_is_reported_as_not_managed() {
        // Decal's own Inject.dll is native; pointing the plugin picker at one
        // should say so rather than produce a nonsense registration.
        let native = include_bytes!("../../helpers/decinject.exe");
        let err = identity_from_bytes(native).unwrap_err();
        assert!(err.contains("not a .NET assembly"), "{err}");
    }

    /// Manual probe against a real managed assembly, since no .NET DLL is
    /// committed to this repo to test against. Point it at anything managed:
    /// `AC_TEST_DLL=… cargo test -p ac-core -- --ignored --nocapture reads_a_real`
    #[test]
    #[ignore = "needs a real .NET assembly on this machine"]
    fn reads_a_real_assembly() {
        let path = std::env::var("AC_TEST_DLL").expect("set AC_TEST_DLL");
        println!("{:#?}", plugin_identity(Path::new(&path)));
    }

    #[test]
    fn a_single_string_attribute_argument_is_decoded() {
        // prolog 0x0001, length 4, "abcd"
        let blob = [0x01, 0x00, 0x04, b'a', b'b', b'c', b'd'];
        assert_eq!(single_string_arg(&blob).as_deref(), Some("abcd"));
        // A null string carries no name.
        assert_eq!(single_string_arg(&[0x01, 0x00, 0xFF]), None);
        // Not an attribute blob at all.
        assert_eq!(single_string_arg(&[0x99, 0x99, 0x01, b'x']), None);
    }
}
