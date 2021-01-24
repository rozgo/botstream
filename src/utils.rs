#[repr(C)]
pub enum BsVideoFormat {
    RGBA8,
}
#[repr(C)]
pub struct BsVideoConfig {
    pub format: BsVideoFormat,
    pub width: u32,
    pub height: u32,
    pub framerate: u32,
}