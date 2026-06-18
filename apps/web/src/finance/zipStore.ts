// Tiny STORE-mode ZIP writer — no deps, no compression.
//
// Why handwritten: the monthly-close package is a handful of small
// CSVs (<100 KB total). Pulling JSZip or a CompressionStream path is
// more weight than the format itself. STORE mode is just local headers
// + raw bytes + central directory. ~80 lines and it round-trips
// cleanly in macOS Finder, Windows Explorer, `unzip`, and Python's
// `zipfile`.
//
// Not a general-purpose ZIP writer: no compression, no ZIP64, no
// encryption, no extra fields, no unicode flag beyond UTF-8 filename
// bytes (every consumer we care about reads those as UTF-8 by
// default). Grow only if a future report needs it.

export type ZipFile = Readonly<{
  name: string;
  content: string;
}>;

/// CRC32 as specified by ZIP (IEEE 802.3 polynomial, reflected). The
/// table is materialised on first call and cached; one malloc, no
/// per-file recompute.
let crcTable: Uint32Array | null = null;
function getCrcTable(): Uint32Array {
  if (crcTable) return crcTable;
  const t = new Uint32Array(256);
  for (let i = 0; i < 256; i++) {
    let c = i;
    for (let k = 0; k < 8; k++) {
      c = c & 1 ? 0xedb88320 ^ (c >>> 1) : c >>> 1;
    }
    t[i] = c >>> 0;
  }
  crcTable = t;
  return t;
}

function crc32(bytes: Uint8Array): number {
  const t = getCrcTable();
  let c = 0xffffffff;
  for (let i = 0; i < bytes.length; i++) {
    c = (t[(c ^ bytes[i]!) & 0xff]! ^ (c >>> 8)) >>> 0;
  }
  return (c ^ 0xffffffff) >>> 0;
}

function writeU16(v: DataView, off: number, n: number): void {
  v.setUint16(off, n, /* littleEndian */ true);
}
function writeU32(v: DataView, off: number, n: number): void {
  v.setUint32(off, n, /* littleEndian */ true);
}

/// Build a ZIP from the given files. All content is stored (not
/// deflated); last-mod timestamps are zeroed (DOS epoch), which every
/// extractor tolerates.
export function buildZip(files: ReadonlyArray<ZipFile>): Blob {
  const encoder = new TextEncoder();
  const parts: Uint8Array[] = [];
  type Entry = {
    nameBytes: Uint8Array;
    crc: number;
    size: number;
    offset: number;
  };
  const entries: Entry[] = [];
  let offset = 0;

  for (const file of files) {
    const nameBytes = encoder.encode(file.name);
    const data = encoder.encode(file.content);
    const crc = crc32(data);
    const header = new Uint8Array(30 + nameBytes.length);
    const hv = new DataView(header.buffer);
    writeU32(hv, 0, 0x04034b50); // local file header signature
    writeU16(hv, 4, 20); // version needed (2.0)
    writeU16(hv, 6, 0); // flags (UTF-8 bit intentionally left off; ASCII filenames here)
    writeU16(hv, 8, 0); // method = store
    writeU16(hv, 10, 0); // mod time
    writeU16(hv, 12, 0); // mod date
    writeU32(hv, 14, crc);
    writeU32(hv, 18, data.length); // compressed size
    writeU32(hv, 22, data.length); // uncompressed size
    writeU16(hv, 26, nameBytes.length);
    writeU16(hv, 28, 0); // extra length
    header.set(nameBytes, 30);

    entries.push({ nameBytes, crc, size: data.length, offset });
    parts.push(header, data);
    offset += header.length + data.length;
  }

  const cdStart = offset;
  for (const e of entries) {
    const cd = new Uint8Array(46 + e.nameBytes.length);
    const cv = new DataView(cd.buffer);
    writeU32(cv, 0, 0x02014b50); // central directory header signature
    writeU16(cv, 4, 20); // version made by
    writeU16(cv, 6, 20); // version needed
    writeU16(cv, 8, 0);
    writeU16(cv, 10, 0);
    writeU16(cv, 12, 0);
    writeU16(cv, 14, 0);
    writeU32(cv, 16, e.crc);
    writeU32(cv, 20, e.size);
    writeU32(cv, 24, e.size);
    writeU16(cv, 28, e.nameBytes.length);
    writeU16(cv, 30, 0); // extra
    writeU16(cv, 32, 0); // comment
    writeU16(cv, 34, 0); // disk number
    writeU16(cv, 36, 0); // internal attrs
    writeU32(cv, 38, 0); // external attrs
    writeU32(cv, 42, e.offset);
    cd.set(e.nameBytes, 46);
    parts.push(cd);
    offset += cd.length;
  }
  const cdSize = offset - cdStart;

  const eocd = new Uint8Array(22);
  const ev = new DataView(eocd.buffer);
  writeU32(ev, 0, 0x06054b50); // EOCD signature
  writeU16(ev, 4, 0); // this disk
  writeU16(ev, 6, 0); // disk with CD start
  writeU16(ev, 8, entries.length); // entries on this disk
  writeU16(ev, 10, entries.length); // total entries
  writeU32(ev, 12, cdSize);
  writeU32(ev, 16, cdStart);
  writeU16(ev, 20, 0); // comment length
  parts.push(eocd);

  // Concat into one Blob. Blob ctor takes an array of BufferSource
  // directly, so no intermediate Uint8Array copy.
  return new Blob(parts as BlobPart[], { type: 'application/zip' });
}
