//! Modelled screw threads: real helical geometry with the ISO 68-1 basic profile,
//! which ASME B1.1 (UNC/UNF) shares.
//!
//! The basic profile is a 60° thread with fundamental triangle height
//! `H = √3/2 · P`. The major diameter is `d`. The minor diameter `D1 = d − 5H/4` is
//! where the flat roots of the internal thread lie. Crest flats are `P/8` wide at
//! `d` and root flats `P/4` wide at `D1`. [`Shape::thread_rod`] builds that profile
//! as a core cylinder of diameter `D1` plus a helical ridge out to `d`. The same
//! solid is the screw's threaded shank and the tool that cuts a threaded hole (the
//! hole's void is the basic external thread). A radial `offset` grows it (`> 0`,
//! clearance for a hole tool) or shrinks it (`< 0`, a looser screw).

use crate::{ffi::bridge as raw, wrap, Error, Result, Shape};

/// ISO 261 coarse pitch for a metric nominal diameter (mm), M1 to M24.
pub fn iso_coarse_pitch(d: f64) -> Option<f64> {
    const T: &[(f64, f64)] = &[
        (1.0, 0.25),
        (1.2, 0.25),
        (1.6, 0.35),
        (2.0, 0.4),
        (2.5, 0.45),
        (3.0, 0.5),
        (3.5, 0.6),
        (4.0, 0.7),
        (5.0, 0.8),
        (6.0, 1.0),
        (8.0, 1.25),
        (10.0, 1.5),
        (12.0, 1.75),
        (14.0, 2.0),
        (16.0, 2.0),
        (20.0, 2.5),
        (24.0, 3.0),
    ];
    T.iter().find(|(n, _)| (n - d).abs() < 1e-9).map(|(_, p)| *p)
}

/// A 60° thread: major diameter and pitch, in mm.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Thread {
    pub major: f64,
    pub pitch: f64,
}

impl Thread {
    /// Parse a designation: `M3` (ISO coarse), `M3x0.35` (fine), `#4-40` (numbered
    /// UN size, threads per inch) or `1/4-20` / `0.25-20` (fractional UN size).
    pub fn parse(s: &str) -> Option<Thread> {
        let s = s.trim();
        if let Some(m) = s.strip_prefix(['M', 'm']) {
            let (d, p) = match m.split_once(['x', 'X', '×']) {
                Some((d, p)) => (d.trim().parse().ok()?, Some(p.trim().parse::<f64>().ok()?)),
                None => (m.parse().ok()?, None),
            };
            let pitch = p.or_else(|| iso_coarse_pitch(d))?;
            return (d > 0.0 && pitch > 0.0).then_some(Thread { major: d, pitch });
        }
        // "1-1/4-7" (whole plus fraction) is not handled: the catalog stops at 1/4-20
        let (size, tpi) = s.split_once('-')?;
        let tpi: f64 = tpi.trim().parse().ok()?;
        let inch = if let Some(n) = size.strip_prefix('#') {
            // ASME B1.1 numbered sizes: d = 0.060 + 0.013 N inches
            0.060 + 0.013 * n.trim().parse::<f64>().ok()?
        } else if let Some((a, b)) = size.split_once('/') {
            a.trim().parse::<f64>().ok()? / b.trim().parse::<f64>().ok()?
        } else {
            size.trim().parse().ok()?
        };
        (inch > 0.0 && tpi > 0.0).then_some(Thread { major: inch * 25.4, pitch: 25.4 / tpi })
    }

    /// Fundamental triangle height `H`.
    pub fn h(&self) -> f64 {
        3f64.sqrt() / 2.0 * self.pitch
    }
    /// Basic minor diameter `D1 = d − 5H/4` (ISO 724: d − 1.082532 P).
    pub fn minor(&self) -> f64 {
        self.major - 1.25 * self.h()
    }
    /// Basic pitch diameter `d2 = d − 3H/4` (d − 0.649519 P).
    pub fn pitch_diameter(&self) -> f64 {
        self.major - 0.75 * self.h()
    }
}

impl Shape {
    /// A threaded rod along +Z from z = 0 to `length`, with the basic profile of `t`
    /// grown radially by `offset` (see the module docs). The thread starts at
    /// angle 0 and is right-handed.
    pub fn thread_rod(t: &Thread, length: f64, offset: f64) -> Result<Shape> {
        let p = t.pitch;
        // a radial offset moves the flanks axially by offset·tan 30° on each side
        let dw = 2.0 * offset * 30f64.to_radians().tan();
        let (w_root, w_crest) = (0.75 * p + dw, p / 8.0 + dw);
        if w_crest < 0.0 || w_root >= p {
            return Err(Error(format!("thread offset {offset} is too large for pitch {p}")));
        }
        wrap(raw::thread_solid(t.minor() / 2.0 + offset, t.major / 2.0 + offset, p, length, w_root, w_crest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn designations() {
        assert_eq!(Thread::parse("M3"), Some(Thread { major: 3.0, pitch: 0.5 }));
        assert_eq!(Thread::parse("M2.5"), Some(Thread { major: 2.5, pitch: 0.45 }));
        assert_eq!(Thread::parse("M8x1"), Some(Thread { major: 8.0, pitch: 1.0 }));
        let t = Thread::parse("#4-40").unwrap();
        assert!((t.major - 0.112 * 25.4).abs() < 1e-9 && (t.pitch - 25.4 / 40.0).abs() < 1e-12);
        let t = Thread::parse("1/4-20").unwrap();
        assert!((t.major - 6.35).abs() < 1e-9 && (t.pitch - 1.27).abs() < 1e-12);
        assert_eq!(Thread::parse("M7"), None, "no coarse pitch");
        assert_eq!(Thread::parse("K30"), None);
        // ISO 724: M3 D1 = 2.459, d2 = 2.675
        let m3 = Thread::parse("M3").unwrap();
        assert!((m3.minor() - 2.4587).abs() < 1e-3 && (m3.pitch_diameter() - 2.6752).abs() < 1e-3);
    }
}
