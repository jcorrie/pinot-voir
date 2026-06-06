use {defmt_rtt as _, panic_probe as _};

pub const BUFFER_SIZE: usize = 960;

#[derive(Clone, Copy)]
pub struct AudioBlock {
    pub samples: [i16; BUFFER_SIZE],
    pub block_id: u32,
    pub timestamp: u64,
}

impl Default for AudioBlock {
    fn default() -> Self {
        Self::new()
    }
}

impl AudioBlock {
    pub fn new() -> Self {
        Self {
            samples: [0; BUFFER_SIZE],
            block_id: 0,
            timestamp: 0,
        }
    }

    pub fn update_samples_from_u16(&mut self, samples: [u16; BUFFER_SIZE]) {
        let samples = samples.map(|x| ((x as i16).wrapping_sub(2048)) << 4);
        self.samples = samples;
    }

    pub fn update_samples_from_u32(&mut self, samples: &[u32; BUFFER_SIZE]) {
        for (dst, &src) in self.samples.iter_mut().zip(samples.iter()) {
            *dst = ((src as i32) >> 16) as i16;
        }
    }
    pub fn centre_samples(&mut self) {
        self.samples = self.samples.map(|x| {
            let centered = (x as i32) - 2048; // widen first, -2048..+2047
            (centered * 16) as i16 // scale into ~-32768..+32752
        });
    }
}
