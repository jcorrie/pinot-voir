const AUDIO_BUFFER_SIZE: usize = 512;

#[derive(Clone, Copy)]
pub struct AudioBlock {
    pub samples: [u16; AUDIO_BUFFER_SIZE],
    pub block_id: u32,
    pub timestamp: u64,
}

impl AudioBlock {
    pub fn new() -> Self {
        Self {
            samples: [0; AUDIO_BUFFER_SIZE],
            block_id: 0,
            timestamp: 0,
        }
    }

    pub fn centre_samples(&self) -> [i16; AUDIO_BUFFER_SIZE] {
        self.samples.map(|x| (x as i16) - 2048)
    }
}
