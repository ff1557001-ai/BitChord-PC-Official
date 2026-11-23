//! The pictures YouTube signs as an m3u8, lifted out of the transport stream they come in
//! and written as an MP4.
//!
//! Every rendition of YouTube's playlist is MPEG-TS, and Media Foundation on Windows has
//! no reader for one: it opens the file and reports the end of the stream before the first
//! picture. The H.264 inside it is the same picture the plain mp4 offers, so it is taken
//! out of the stream and put in a file of our own making — video only, since the song's
//! sound comes from its own stream and a second track here would only be a copy of it.

/// The remux, fed the transport stream a piece at a time. It hands back the pictures to
/// append to the file's `mdat`, and at the end the header that describes them.
pub struct Remux {
    /// The program map's PID, then the video's: the stream is read once they are known.
    pmt: u16,
    video: u16,
    /// The 188-byte packet being gathered; a piece boundary can fall anywhere in it.
    packet: Vec<u8>,
    /// The picture packet being gathered, and the clock it carries.
    pes: Vec<u8>,
    /// The access unit being gathered, as the length-prefixed pictures an MP4 holds.
    au: Vec<u8>,
    au_pts: Option<u64>,
    au_sync: bool,
    au_vcl: bool,
    samples: Vec<Sample>,
    sps: Option<Vec<u8>>,
    pps: Option<Vec<u8>>,
    size: (u32, u32),
}

/// One picture of the finished file: how many bytes of the `mdat` it takes, where it falls
/// on the stream's clock, and whether it can be decoded on its own.
struct Sample {
    size: u32,
    pts: Option<u64>,
    sync: bool,
}

/// The stream's clock: 90 kHz, which is what its picture packets are stamped in.
const SCALE: u32 = 90_000;

impl Remux {
    /// `size` is the picture's size as YouTube named it, kept for the case where the
    /// stream's own header goes unread.
    pub fn new(width: u32, height: u32) -> Self {
        Self {
            pmt: 0,
            video: 0,
            packet: Vec::with_capacity(188),
            pes: Vec::new(),
            au: Vec::new(),
            au_pts: None,
            au_sync: false,
            au_vcl: false,
            samples: Vec::new(),
            sps: None,
            pps: None,
            size: (width, height),
        }
    }

    /// Take in the next piece of the transport stream; hand back the pictures it held,
    /// ready to be appended to the file's `mdat`.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        for &b in bytes {
            // A packet starts on its sync byte, so anything between two pieces — or a
            // piece that lands mid-packet — is dropped until one turns up again.
            if self.packet.is_empty() && b != 0x47 {
                continue;
            }
            self.packet.push(b);
            if self.packet.len() == 188 {
                let packet = std::mem::take(&mut self.packet);
                self.packet(packet, &mut out);
            }
        }
        out
    }

    /// End the stream: the last picture, then the header that describes them all.
    /// `first_sample_at` is where the first picture sits in the finished file, which is
    /// the one place the header has to know about the file around it.
    pub fn finish(&mut self, first_sample_at: u64) -> Option<(Vec<u8>, Vec<u8>)> {
        let mut tail = Vec::new();
        let pes = std::mem::take(&mut self.pes);
        self.picture(&pes, &mut tail);
        self.end_au(&mut tail);
        let (sps, pps) = (self.sps.clone()?, self.pps.clone()?);
        if self.samples.is_empty() {
            return None;
        }
        if let Some(size) = sps_size(&sps) {
            self.size = size;
        }
        Some((tail, self.moov(&sps, &pps, first_sample_at)))
    }

    /// One transport-stream packet: the program map teaches which packets carry pictures,
    /// and those are gathered into the picture packet they belong to.
    fn packet(&mut self, p: Vec<u8>, out: &mut Vec<u8>) {
        if p[0] != 0x47 {
            return;
        }
        let fresh = p[1] & 0x40 != 0;
        let pid = (((p[1] as u16) & 0x1F) << 8) | p[2] as u16;
        let mut at = 4usize;
        if p[3] & 0x20 != 0 {
            at += 1 + p[4] as usize;
        }
        if at >= p.len() {
            return;
        }
        let payload = &p[at..];
        if pid == 0 && self.pmt == 0 {
            self.pmt = pat(payload).unwrap_or(0);
        } else if self.pmt != 0 && pid == self.pmt && self.video == 0 {
            self.video = pmt(payload).unwrap_or(0);
        } else if self.video != 0 && pid == self.video {
            // A packet marked as a start ends the picture packet before it: what follows
            // belongs to a picture of its own.
            if fresh {
                let pes = std::mem::take(&mut self.pes);
                self.picture(&pes, out);
            }
            self.pes.extend_from_slice(payload);
        }
    }

    /// One picture packet: its clock, and the pictures inside it.
    fn picture(&mut self, pes: &[u8], out: &mut Vec<u8>) {
        let Some((pts, data)) = pes_body(pes) else { return };
        nals(data, |nal| self.nal(nal, pts, out));
    }

    /// One picture of the stream. The parameter sets and the slice pictures are gathered
    /// into the access unit they belong to, and a slice that starts a frame ends the unit
    /// before it: that boundary is what an MP4 calls a sample.
    fn nal(&mut self, nal: &[u8], pts: Option<u64>, out: &mut Vec<u8>) {
        if nal.is_empty() {
            return;
        }
        match nal[0] & 0x1F {
            7 if self.sps.is_none() => self.sps = Some(nal.to_vec()),
            8 if self.pps.is_none() => self.pps = Some(nal.to_vec()),
            _ => {}
        }
        let vcl = (1..=5).contains(&(nal[0] & 0x1F));
        if vcl && self.au_vcl && starts_a_frame(nal) {
            self.end_au(out);
        }
        self.au.extend_from_slice(&(nal.len() as u32).to_be_bytes());
        self.au.extend_from_slice(nal);
        if self.au_pts.is_none() {
            self.au_pts = pts;
        }
        if vcl {
            self.au_vcl = true;
            self.au_sync |= nal[0] & 0x1F == 5;
        }
    }

    /// Write the access unit out as the next sample. One that holds no slice is only the
    /// parameter sets of the picture after it, so it stays gathered.
    fn end_au(&mut self, out: &mut Vec<u8>) {
        if !self.au_vcl || self.au.is_empty() {
            return;
        }
        self.samples.push(Sample { size: self.au.len() as u32, pts: self.au_pts, sync: self.au_sync });
        out.append(&mut self.au);
        self.au = Vec::new();
        self.au_pts = None;
        self.au_sync = false;
        self.au_vcl = false;
    }

    /// The header of the finished file: one video track, every picture of it in one run of
    /// the `mdat` that precedes it.
    fn moov(&self, sps: &[u8], pps: &[u8], first_sample_at: u64) -> Vec<u8> {
        let deltas = durations(&self.samples);
        let total: u64 = deltas.iter().map(|d| *d as u64).sum();
        let (w, h) = self.size;

        let avcc = {
            let mut b = vec![1u8, sps.get(1).copied().unwrap_or(0), sps.get(2).copied().unwrap_or(0), sps.get(3).copied().unwrap_or(0), 0xFF, 0xE1];
            b.extend_from_slice(&(sps.len() as u16).to_be_bytes());
            b.extend_from_slice(sps);
            b.push(1);
            b.extend_from_slice(&(pps.len() as u16).to_be_bytes());
            b.extend_from_slice(pps);
            box_of(b"avcC", &b)
        };
        let mut avc1 = Vec::new();
        avc1.extend_from_slice(&[0u8; 6]);
        avc1.extend_from_slice(&1u16.to_be_bytes());
        avc1.extend_from_slice(&[0u8; 16]);
        avc1.extend_from_slice(&(w.min(0xFFFF) as u16).to_be_bytes());
        avc1.extend_from_slice(&(h.min(0xFFFF) as u16).to_be_bytes());
        avc1.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        avc1.extend_from_slice(&0x0048_0000u32.to_be_bytes());
        avc1.extend_from_slice(&0u32.to_be_bytes());
        avc1.extend_from_slice(&1u16.to_be_bytes());
        let mut name = [0u8; 32];
        name[0] = 10;
        name[1..11].copy_from_slice(b"BitChord  ");
        avc1.extend_from_slice(&name);
        avc1.extend_from_slice(&0x18u16.to_be_bytes());
        avc1.extend_from_slice(&(-1i16).to_be_bytes());
        avc1.extend_from_slice(&avcc);

        let stsd = fullbox(b"stsd", 0, &cat(&[&1u32.to_be_bytes(), &box_of(b"avc1", &avc1)]));
        let stts = {
            let mut runs: Vec<(u32, u32)> = Vec::new();
            for d in &deltas {
                match runs.last_mut() {
                    Some((n, delta)) if *delta == *d => *n += 1,
                    _ => runs.push((1, *d)),
                }
            }
            let mut body = (runs.len() as u32).to_be_bytes().to_vec();
            for (n, d) in runs {
                body.extend_from_slice(&n.to_be_bytes());
                body.extend_from_slice(&d.to_be_bytes());
            }
            fullbox(b"stts", 0, &body)
        };
        let sync: Vec<u32> = self.samples.iter().enumerate().filter(|(_, s)| s.sync).map(|(i, _)| (i + 1) as u32).collect();
        let stss = if sync.is_empty() || sync.len() == self.samples.len() {
            // Every picture a key picture is worth no table at all, and neither is none.
            Vec::new()
        } else {
            let mut body = (sync.len() as u32).to_be_bytes().to_vec();
            for n in sync {
                body.extend_from_slice(&n.to_be_bytes());
            }
            fullbox(b"stss", 0, &body)
        };
        let stsc = fullbox(b"stsc", 0, &cat(&[&1u32.to_be_bytes(), &1u32.to_be_bytes(), &(self.samples.len() as u32).to_be_bytes(), &1u32.to_be_bytes()]));
        let stsz = {
            let mut body = 0u32.to_be_bytes().to_vec();
            body.extend_from_slice(&(self.samples.len() as u32).to_be_bytes());
            for s in &self.samples {
                body.extend_from_slice(&s.size.to_be_bytes());
            }
            fullbox(b"stsz", 0, &body)
        };
        let stco = match u32::try_from(first_sample_at) {
            Ok(at) => fullbox(b"stco", 0, &cat(&[&1u32.to_be_bytes(), &at.to_be_bytes()])),
            Err(_) => fullbox(b"co64", 0, &cat(&[&1u32.to_be_bytes(), &first_sample_at.to_be_bytes()])),
        };

        let stbl = cat(&[&stsd, &stts, &stss, &stsc, &stsz, &stco]);
        let dinf = box_of(b"dinf", &fullbox(b"dref", 0, &cat(&[&1u32.to_be_bytes(), &fullbox(b"url ", 1, &[])])));
        let vmhd = fullbox(b"vmhd", 1, &cat(&[&0u16.to_be_bytes(), &[0u8; 6]]));
        let minf = box_of(b"minf", &cat(&[&vmhd, &dinf, &box_of(b"stbl", &stbl)]));
        let span = total.min(u32::MAX as u64) as u32;
        let mdhd = fullbox(b"mdhd", 0, &cat(&[&0u32.to_be_bytes(), &0u32.to_be_bytes(), &SCALE.to_be_bytes(), &span.to_be_bytes(), &0x55C4u16.to_be_bytes(), &0u16.to_be_bytes()]));
        let hdlr = fullbox(b"hdlr", 0, &cat(&[&0u32.to_be_bytes(), b"vide", &[0u8; 12], b"VideoHandler\0"]));
        let mdia = box_of(b"mdia", &cat(&[&mdhd, &hdlr, &minf]));
        let tkhd = fullbox(
            b"tkhd",
            3,
            &cat(&[
                &0u32.to_be_bytes(),
                &0u32.to_be_bytes(),
                &1u32.to_be_bytes(),
                &0u32.to_be_bytes(),
                &span.to_be_bytes(),
                &[0u8; 8],
                &0u16.to_be_bytes(),
                &0u16.to_be_bytes(),
                &0u16.to_be_bytes(),
                &0u16.to_be_bytes(),
                &matrix(),
                &(w << 16).to_be_bytes(),
                &(h << 16).to_be_bytes(),
            ]),
        );
        let trak = box_of(b"trak", &cat(&[&tkhd, &mdia]));
        let mvhd = fullbox(
            b"mvhd",
            0,
            &cat(&[
                &0u32.to_be_bytes(),
                &0u32.to_be_bytes(),
                &SCALE.to_be_bytes(),
                &span.to_be_bytes(),
                &0x0001_0000u32.to_be_bytes(),
                &0x0100u16.to_be_bytes(),
                &[0u8; 10],
                &matrix(),
                &[0u8; 24],
                &2u32.to_be_bytes(),
            ]),
        );
        box_of(b"moov", &cat(&[&mvhd, &trak]))
    }

    /// The file's first box, naming the kind of file it is. It is written before the
    /// `mdat` the pictures land in, while they are still coming in.
    pub fn header() -> Vec<u8> {
        let mut ftyp = b"isom".to_vec();
        ftyp.extend_from_slice(&0x200u32.to_be_bytes());
        for kind in [b"isom", b"iso2", b"avc1", b"mp41"] {
            ftyp.extend_from_slice(kind);
        }
        box_of(b"ftyp", &ftyp)
    }
}

/// The pieces of a box's body, in order.
fn cat(parts: &[&[u8]]) -> Vec<u8> {
    let mut b = Vec::with_capacity(parts.iter().map(|p| p.len()).sum());
    for p in parts {
        b.extend_from_slice(p);
    }
    b
}

/// The picture's own matrix: no rotation, no scaling.
fn matrix() -> Vec<u8> {
    let rows: [u32; 9] = [0x0001_0000, 0, 0, 0, 0x0001_0000, 0, 0, 0, 0x4000_0000];
    rows.iter().flat_map(|r| r.to_be_bytes()).collect()
}

/// One box: its length, its name and its body.
fn box_of(name: &[u8; 4], body: &[u8]) -> Vec<u8> {
    let mut b = Vec::with_capacity(body.len() + 8);
    b.extend_from_slice(&((body.len() + 8) as u32).to_be_bytes());
    b.extend_from_slice(name);
    b.extend_from_slice(body);
    b
}

/// One box with the version and flags every table starts with.
fn fullbox(name: &[u8; 4], flags: u32, body: &[u8]) -> Vec<u8> {
    let mut b = (flags & 0x00FF_FFFF).to_be_bytes().to_vec();
    b[0] = 0;
    b.extend_from_slice(body);
    box_of(name, &b)
}

/// How long each picture holds the screen. Only the packet that starts a frame carries a
/// stamp, so the distance between two stamps is shared out between the pictures they
/// cover — a frame of several slices is still one moment on the clock.
fn durations(samples: &[Sample]) -> Vec<u32> {
    let mut out = vec![0u32; samples.len()];
    let mut from = 0usize;
    let mut prev: Option<u64> = None;
    for (i, s) in samples.iter().enumerate() {
        let Some(pts) = s.pts else { continue };
        if let Some(last) = prev {
            let span = pts.saturating_sub(last);
            let n = (i - from) as u64;
            if n > 0 && span > 0 {
                let d = (span / n).clamp(1, u32::MAX as u64) as u32;
                out[from..i].fill(d);
            }
        }
        prev = Some(pts);
        from = i;
    }
    // The last stretch has nothing after it to be measured against: the pace it found is
    // the pace it keeps.
    let tail = out[..from].iter().rev().find(|d| **d > 0).copied().unwrap_or(SCALE / 25);
    for d in out.iter_mut() {
        if *d == 0 {
            *d = tail;
        }
    }
    out
}

/// Whether this slice starts a frame: its first macroblock is the frame's first, so the
/// slices after it are the rest of the picture that is already being gathered.
fn starts_a_frame(nal: &[u8]) -> bool {
    Bits::new(&nal[1..]).ue() == 0
}

/// The picture packet's clock and its pictures: only the video packets of a stream carry
/// what this file needs.
fn pes_body(data: &[u8]) -> Option<(Option<u64>, &[u8])> {
    if data.len() < 10 || data[0] != 0 || data[1] != 0 || data[2] != 1 {
        return None;
    }
    if !(0xE0..=0xEF).contains(&data[3]) {
        return None;
    }
    let head = 9 + data[8] as usize;
    if data.len() <= head {
        return None;
    }
    let b = &data[9..];
    let pts = (data[7] >> 6 >= 2 && b.len() >= 5).then(|| {
        (((b[0] as u64) >> 1 & 7) << 30) | ((b[1] as u64) << 22) | (((b[2] as u64) >> 1 & 0x7F) << 15) | ((b[3] as u64) << 7) | ((b[4] as u64) >> 1)
    });
    Some((pts, &data[head..]))
}

/// The program map's PID, out of the stream's first table.
fn pat(data: &[u8]) -> Option<u16> {
    let s = section(data, 0)?;
    let end = table_end(s);
    let mut i = 8;
    while i + 3 < end {
        let program = u16::from_be_bytes([s[i], s[i + 1]]);
        let pid = u16::from_be_bytes([s[i + 2], s[i + 3]]) & 0x1FFF;
        if program != 0 {
            return Some(pid);
        }
        i += 4;
    }
    None
}

/// The PID of the program's H.264 pictures.
fn pmt(data: &[u8]) -> Option<u16> {
    let s = section(data, 2)?;
    let end = table_end(s);
    let info = u16::from_be_bytes([s[10], s[11]]) as usize & 0x0FFF;
    let mut i = 12 + info;
    while i + 4 < end {
        let kind = s[i];
        let pid = u16::from_be_bytes([s[i + 1], s[i + 2]]) & 0x1FFF;
        let es = u16::from_be_bytes([s[i + 3], s[i + 4]]) as usize & 0x0FFF;
        if kind == 0x1B {
            return Some(pid);
        }
        i += 5 + es;
    }
    None
}

/// A table's body, past the pointer field of the packet that carried it.
fn section(data: &[u8], table: u8) -> Option<&[u8]> {
    let at = 1 + (*data.first()?) as usize;
    let s = data.get(at..)?;
    (s.first() == Some(&table) && s.len() > 12).then_some(s)
}

/// Where a table's entries stop: its own length, less the checksum it ends with.
fn table_end(s: &[u8]) -> usize {
    let len = (((s[1] as usize) & 0x0F) << 8) | s[2] as usize;
    (3 + len).min(s.len()).saturating_sub(4)
}

/// Walk the pictures of an elementary stream, start codes stripped.
fn nals(data: &[u8], mut each: impl FnMut(&[u8])) {
    let mut start: Option<usize> = None;
    let mut i = 0;
    while i + 3 <= data.len() {
        let head = if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            3
        } else if i + 4 <= data.len() && data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 1 {
            4
        } else {
            i += 1;
            continue;
        };
        if let Some(s) = start {
            // A start code's leading zeros belong to it, not to the picture before it.
            let mut end = i;
            while end > s && data[end - 1] == 0 {
                end -= 1;
            }
            each(&data[s..end]);
        }
        start = Some(i + head);
        i += head;
    }
    if let Some(s) = start {
        each(&data[s..]);
    }
}

/// The picture's size, out of the parameter set that describes it.
fn sps_size(nal: &[u8]) -> Option<(u32, u32)> {
    let body = rbsp(&nal[1..]);
    let mut b = Bits::new(&body);
    let profile = b.u(8);
    b.u(16);
    b.ue();
    let mut chroma = 1;
    let mut separate = false;
    if matches!(profile, 100 | 110 | 122 | 244 | 44 | 83 | 86 | 118 | 128 | 138 | 139 | 134 | 135) {
        chroma = b.ue();
        if chroma == 3 {
            separate = b.u1() == 1;
        }
        b.ue();
        b.ue();
        b.u1();
        if b.u1() == 1 {
            for i in 0..if chroma == 3 { 12 } else { 8 } {
                if b.u1() == 1 {
                    scaling_list(&mut b, if i < 6 { 16 } else { 64 });
                }
            }
        }
    }
    b.ue();
    match b.ue() {
        0 => {
            b.ue();
        }
        1 => {
            b.u1();
            b.se();
            b.se();
            for _ in 0..b.ue() {
                b.se();
            }
        }
        _ => {}
    }
    b.ue();
    b.u1();
    let width = (b.ue() + 1) * 16;
    let height = (b.ue() + 1) * 16;
    let frames_only = b.u1() == 1;
    if !frames_only {
        b.u1();
    }
    b.u1();
    let interlaced = if frames_only { 1 } else { 2 };
    let mut size = (width, interlaced * height);
    if b.u1() == 1 {
        let (l, r, t, bot) = (b.ue(), b.ue(), b.ue(), b.ue());
        let cx = if chroma == 3 && !separate { 1 } else { 2 };
        let cy = interlaced * if chroma == 1 { 2 } else { 1 };
        size = (width - (l + r) * cx, interlaced * height - (t + bot) * cy);
    }
    (size.0 > 0 && size.1 > 0).then_some(size)
}

/// Skip a quantisation table: its values are of no use to a file that only carries them.
fn scaling_list(b: &mut Bits, size: usize) {
    let mut last = 8i32;
    for _ in 0..size {
        let delta = b.se();
        let value = if delta != 0 { delta } else { last };
        if value == 0 {
            return;
        }
        last = value;
    }
}

/// A picture's bytes without the emulation prevention the stream puts in them.
fn rbsp(nal: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(nal.len());
    let mut zeros = 0;
    for &b in nal {
        if zeros >= 2 && b == 3 {
            zeros = 0;
            continue;
        }
        zeros = if b == 0 { zeros + 1 } else { 0 };
        out.push(b);
    }
    out
}

/// A reader of the single bits an H.264 header is written in.
struct Bits<'a> {
    b: &'a [u8],
    at: usize,
}

impl<'a> Bits<'a> {
    fn new(b: &'a [u8]) -> Self {
        Self { b, at: 0 }
    }

    fn u1(&mut self) -> u32 {
        let byte = *self.b.get(self.at / 8).unwrap_or(&0);
        self.at += 1;
        ((byte >> (7 - ((self.at - 1) % 8))) & 1) as u32
    }

    fn u(&mut self, n: usize) -> u32 {
        let mut v = 0;
        for _ in 0..n {
            v = (v << 1) | self.u1();
        }
        v
    }

    /// An unsigned Exp-Golomb code.
    fn ue(&mut self) -> u32 {
        let mut zeros = 0;
        while self.u1() == 0 && zeros < 31 {
            zeros += 1;
        }
        (1 << zeros) - 1 + self.u(zeros)
    }

    /// A signed Exp-Golomb code.
    fn se(&mut self) -> i32 {
        let k = self.ue();
        if k % 2 == 0 {
            -((k / 2) as i32)
        } else {
            ((k + 1) / 2) as i32
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The parameter sets of a real 1280x720 clip, as the stream carries them.
    const SPS: [u8; 28] = [0x67, 0x4d, 0x40, 0x1f, 0xe8, 0x80, 0x28, 0x02, 0xdd, 0x80, 0xb5, 0x01, 0x01, 0x01, 0x40, 0x00, 0x00, 0x03, 0x00, 0x40, 0x00, 0x00, 0x0c, 0x83, 0xc6, 0x0c, 0x44, 0x80];
    const PPS: [u8; 4] = [0x68, 0xeb, 0xef, 0x20];

    /// One transport-stream packet of a payload.
    fn packet(pid: u16, fresh: bool, payload: &[u8]) -> Vec<u8> {
        let mut p = vec![0x47, ((pid >> 8) as u8) & 0x1F | if fresh { 0x40 } else { 0 }, (pid & 0xFF) as u8, 0x10];
        p.extend_from_slice(payload);
        p.resize(188, 0xFF);
        p
    }

    /// A slice picture: `kind` 5 is a key picture, and `first` is the macroblock it starts at.
    fn slice(kind: u8, first: u32, len: usize) -> Vec<u8> {
        let mut nal = vec![0x60 | kind];
        // first_mb_in_slice 0 is a single set bit; any other value opens with a zero.
        let code = if first == 0 { vec![0x80u8] } else { vec![0x40u8] };
        nal.extend_from_slice(&code);
        nal.extend_from_slice(&vec![7u8; len]);
        nal
    }

    /// The start code and the picture after it.
    fn annex(nal: &[u8]) -> Vec<u8> {
        let mut b = vec![0, 0, 0, 1];
        b.extend_from_slice(nal);
        b
    }

    /// A picture packet carrying one access unit, stamped at `pts`.
    fn pes(pts: u64, unit: &[u8]) -> Vec<u8> {
        let mut b = vec![0, 0, 1, 0xE0, 0, 0, 0x80, 0x80, 5];
        b.push((0x21 | ((pts >> 29) & 0x0E)) as u8);
        b.push(((pts >> 22) & 0xFF) as u8);
        b.push((0x01 | ((pts >> 14) & 0xFE)) as u8);
        b.push(((pts >> 7) & 0xFF) as u8);
        b.push((0x01 | ((pts << 1) & 0xFE)) as u8);
        b.extend_from_slice(unit);
        let len = b.len() - 6;
        b[4] = (len >> 8) as u8;
        b[5] = (len & 0xFF) as u8;
        b
    }

    /// One access unit: the given pictures in order, stamped at `pts`.
    fn unit(pts: u64, first: &[Vec<u8>]) -> Vec<u8> {
        let mut b = Vec::new();
        for n in first {
            b.extend_from_slice(&annex(n));
        }
        pes(pts, &b)
    }

    /// A program map that names the pictures' PID, as the stream's own tables do.
    fn tables(video_pid: u16) -> Vec<u8> {
        let pat = {
            let mut s = vec![0x00, 0xB0, 13, 0, 1, 0xC1, 0, 0, 0, 1, 0xE1, 0x00, 0, 0, 0, 0];
            s[11] = 0x00;
            let mut p = vec![0x00];
            p.append(&mut s);
            packet(0, true, &p)
        };
        let pmt = {
            let body = vec![0x02, 0xB0, 18, 0, 1, 0xC1, 0, 0, 0xE1, 0x01, 0xF0, 0, 0x1B, ((video_pid >> 8) & 0x1F) as u8 | 0xE0, (video_pid & 0xFF) as u8, 0xF0, 0, 0, 0, 0, 0];
            let mut p = vec![0x00];
            p.extend_from_slice(&body);
            packet(0x100, true, &p)
        };
        [pat, pmt].concat()
    }

    #[test]
    fn the_pictures_of_a_playlist_come_out_as_an_mp4() {
        // A parameter set, then a key picture of two slices, then a plain one: three
        // packets, two samples, and a header that names the track.
        let sps = SPS.to_vec();
        let pps = PPS.to_vec();
        let mut stream = tables(0x101);
        stream.extend_from_slice(&packet(0x101, true, &unit(90_000, &[sps.clone(), pps.clone(), slice(5, 0, 40), slice(5, 1, 10)])));
        stream.extend_from_slice(&packet(0x101, true, &unit(93_600, &[slice(1, 0, 20)])));

        let mut r = Remux::new(0, 0);
        let mut body = r.push(&stream);
        let (tail, moov) = r.finish(32).expect("no mp4 in the stream");
        body.extend_from_slice(&tail);

        assert_eq!(r.samples.len(), 2, "the slices of one frame are one sample");
        assert_eq!(body.len() as u32, r.samples.iter().map(|s| s.size).sum::<u32>(), "the samples cover the pictures");
        assert!(r.samples[0].sync && !r.samples[1].sync, "only the key picture is a key sample");
        // The second frame is 3600 ticks on: 25 pictures a second.
        assert_eq!(durations(&r.samples), vec![3600, 3600]);
        assert_eq!(r.size, (1280, 720), "the parameter set names the picture's size");
        let head = [Remux::header(), moov].concat();
        let text = String::from_utf8_lossy(&head);
        for kind in ["ftyp", "moov", "trak", "avc1", "avcC", "stts", "stsz", "stco"] {
            assert!(text.contains(kind), "the header has no {kind}");
        }
        // A key picture of two slices and its parameter sets, all of them in the sample.
        assert!(head.windows(4).any(|w| w == b"stss"), "the key pictures are listed");
    }

    #[test]
    fn a_stream_with_no_pictures_in_it_makes_no_file() {
        let mut r = Remux::new(1280, 720);
        r.push(&tables(0x101));
        assert!(r.finish(32).is_none());
        let mut r = Remux::new(1280, 720);
        r.push(&[0u8; 188 * 4]);
        assert!(r.finish(32).is_none());
    }

    #[test]
    fn a_piece_boundary_falls_where_it_likes() {
        // The same stream fed a byte at a time makes the same file: the packet and the
        // picture packet are gathered across the boundaries.
        let sps = SPS.to_vec();
        let pps = PPS.to_vec();
        let mut unit = annex(&sps);
        unit.extend_from_slice(&annex(&pps));
        unit.extend_from_slice(&annex(&slice(5, 0, 40)));
        let mut stream = tables(0x101);
        stream.extend_from_slice(&packet(0x101, true, &pes(90_000, &unit)));

        let whole = {
            let mut r = Remux::new(0, 0);
            let mut body = r.push(&stream);
            let (tail, _) = r.finish(32).expect("no mp4");
            body.extend_from_slice(&tail);
            (r.samples.len(), body)
        };
        let drip = {
            let mut r = Remux::new(0, 0);
            let mut body = Vec::new();
            for chunk in stream.chunks(7) {
                body.extend_from_slice(&r.push(chunk));
            }
            let (tail, _) = r.finish(32).expect("no mp4");
            body.extend_from_slice(&tail);
            (r.samples.len(), body)
        };
        assert_eq!(whole.0, 1);
        assert_eq!(whole.1, drip.1);
    }

    #[test]
    fn the_parameter_sets_are_kept_with_the_picture_they_describe() {
        let sps = vec![0x67, 0x4D, 0x40, 0x1F];
        let pps = PPS.to_vec();
        let mut stream = tables(0x101);
        stream.extend_from_slice(&packet(0x101, true, &pes(0, &[annex(&sps), annex(&pps)].concat())));
        let mut r = Remux::new(0, 0);
        r.push(&stream);
        assert!(r.finish(32).is_none(), "a parameter set is no picture on its own");
    }
}
