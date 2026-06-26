//! Doom-fire engine — the classic PSX DOOM fire effect, ported to a
//! character grid and *fueled by live token burn*.
//!
//! The field is a row-major grid of heat intensities (`0..=MAX`). The
//! bottom row is the fuel source; its intensity is set every frame from
//! the current token burn rate. Each step, every cell takes the heat of
//! the cell below it, minus a small random cooldown, shifted randomly
//! left/right — producing a rising, flickering flame. Heat maps to a
//! true-color gradient (black → red → orange → yellow → white) and an
//! ASCII/box glyph ramp, so a hot base reads as solid flame and the
//! cool tips flicker as sparse embers.
//!
//! Token bursts inject transient "flares" — bright columns that whoosh
//! up the screen, so every assistant turn is visible as a lick of flame.

use ratatui::style::Color;

/// Number of heat levels. The classic effect uses 37 (0..=36).
pub const MAX: u8 = 36;

pub struct FireField {
    w: usize,
    h: usize,
    /// Heat grid, row-major. Row 0 is the top (coolest tips), row `h-1`
    /// is the bottom fuel source.
    cells: Vec<u8>,
    rng: u64,
    palette: [(char, Color); (MAX as usize) + 1],
}

impl FireField {
    pub fn new() -> Self {
        Self {
            w: 0,
            h: 0,
            cells: Vec::new(),
            rng: 0x2545_F491_4F6C_DD1D,
            palette: build_palette(),
        }
    }

    #[inline]
    fn rand(&mut self) -> u64 {
        // xorshift64* — tiny inline PRNG, no extra dependency.
        let mut x = self.rng;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.rng = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }

    /// Ensure the grid matches the render area. Re-allocates (cold start
    /// of the fire) only when the size actually changes.
    pub fn resize(&mut self, w: usize, h: usize) {
        if w == self.w && h == self.h {
            return;
        }
        self.w = w;
        self.h = h;
        self.cells = vec![0u8; w * h];
    }

    pub fn dims(&self) -> (usize, usize) {
        (self.w, self.h)
    }

    /// Set the bottom fuel row from a normalized fuel level (0..=1).
    /// A small floor keeps a living "pilot light" even at idle, and the
    /// level climbs to a roaring base as the burn rate rises.
    pub fn set_fuel(&mut self, fuel: f64) {
        if self.w == 0 || self.h == 0 {
            return;
        }
        let fuel = fuel.clamp(0.0, 1.0);
        let base = 0.06 + 0.94 * fuel;
        let bottom = (self.h - 1) * self.w;
        for x in 0..self.w {
            // Per-cell flicker so the base shimmers instead of being a
            // flat bar.
            let jitter = (self.rand() & 0x3F) as f64 / 63.0; // 0..1
            let v = base * (0.80 + 0.20 * jitter);
            self.cells[bottom + x] = (v * MAX as f64).round().clamp(0.0, MAX as f64) as u8;
        }
    }

    /// Inject a transient flare — a hot column a few cells wide near the
    /// base — representing a burst of tokens. `strength` in 0..1 widens
    /// and intensifies the flare.
    pub fn flare(&mut self, strength: f64) {
        if self.w < 2 || self.h < 3 {
            return;
        }
        let strength = strength.clamp(0.0, 1.0);
        let cx = (self.rand() as usize) % self.w;
        let half = 1 + (strength * 4.0) as usize;
        let rows = 1 + (strength * 2.0) as usize; // seed a couple of rows up
        for dy in 0..rows {
            if dy + 1 >= self.h {
                break;
            }
            let y = self.h - 1 - dy;
            for dx in 0..=half {
                for &sx in &[cx.wrapping_sub(dx), cx + dx] {
                    if sx < self.w {
                        let idx = y * self.w + sx;
                        let falloff = 1.0 - (dx as f64 / (half as f64 + 1.0));
                        let v = MAX as f64 * (0.6 + 0.4 * falloff);
                        if (v as u8) > self.cells[idx] {
                            self.cells[idx] = v as u8;
                        }
                    }
                }
            }
        }
    }

    /// Advance the simulation one frame: propagate heat upward with a
    /// random cooldown and lateral shift.
    pub fn step(&mut self) {
        let (w, h) = (self.w, self.h);
        if w == 0 || h < 2 {
            return;
        }
        // Iterate every cell except the bottom source row, pulling heat
        // up from the row below (classic DOOM fire propagation).
        for x in 0..w {
            for y in 1..h {
                let src = y * w + x;
                let pixel = self.cells[src];
                let r = self.rand();
                // Random horizontal drift: +1, 0, -1, -2 — biased so the
                // flame leans and curls instead of rising straight.
                let shift = 1 - (r & 0x3) as i32;
                let nx = (x as i32 + shift).clamp(0, w as i32 - 1) as usize;
                // Cooling 0..2 (avg ~1) carves the fire into distinct
                // licking tongues with dark gaps, rather than a wall.
                let decay = ((r >> 8) % 3) as u8;
                let nv = pixel.saturating_sub(decay);
                self.cells[(y - 1) * w + nx] = nv;
            }
        }
    }

    #[inline]
    pub fn glyph_color(&self, x: usize, y: usize) -> (char, Color) {
        let i = self.cells[y * self.w + x] as usize;
        self.palette[i]
    }
}

impl Default for FireField {
    fn default() -> Self {
        Self::new()
    }
}

/// Precompute the heat → (glyph, color) lookup table.
fn build_palette() -> [(char, Color); (MAX as usize) + 1] {
    // Control stops for the fire gradient: (t, r, g, b).
    const STOPS: &[(f64, f64, f64, f64)] = &[
        (0.00, 0.0, 0.0, 0.0),
        (0.10, 24.0, 0.0, 0.0),
        (0.22, 92.0, 0.0, 0.0),
        (0.35, 160.0, 20.0, 0.0),
        (0.50, 216.0, 56.0, 0.0),
        (0.64, 246.0, 112.0, 8.0),
        (0.76, 252.0, 162.0, 22.0),
        (0.88, 255.0, 206.0, 62.0),
        (1.00, 255.0, 246.0, 204.0),
    ];

    let mut out = [(' ', Color::Black); (MAX as usize) + 1];
    for i in 0..=MAX as usize {
        let t = i as f64 / MAX as f64;
        // Find surrounding stops and interpolate.
        let mut c = (STOPS[0].1, STOPS[0].2, STOPS[0].3);
        for w in STOPS.windows(2) {
            let (t0, r0, g0, b0) = w[0];
            let (t1, r1, g1, b1) = w[1];
            if t >= t0 && t <= t1 {
                let f = if (t1 - t0).abs() < f64::EPSILON {
                    0.0
                } else {
                    (t - t0) / (t1 - t0)
                };
                c = (
                    r0 + (r1 - r0) * f,
                    g0 + (g1 - g0) * f,
                    b0 + (b1 - b0) * f,
                );
                break;
            }
        }
        let color = Color::Rgb(c.0 as u8, c.1 as u8, c.2 as u8);
        // Glyph by intensity bucket: a solid blocky body, a textured
        // mid, sparse ember tips, and a faint pilot-light dot at the very
        // bottom of the range. i == 0 is black "sky".
        let glyph = match i {
            0 => ' ',
            1..=2 => '.',
            3..=5 => ':',
            6..=9 => '*',
            10..=14 => 'o',
            15..=20 => '▒',
            21..=28 => '▓',
            _ => '█',
        };
        out[i] = (glyph, color);
    }
    out
}
