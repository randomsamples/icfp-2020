#![allow(clippy::too_many_arguments)]

use std::convert::TryFrom;
use std::io::{self, Write};

use byteorder::{BigEndian, WriteBytesExt};
use num_iter::range_step;

use crate::{Bgr, Bgra, ColorType, GenericImageView, ImageBuffer, Luma, LumaA, Pixel, Rgb, Rgba};
use crate::error::{ImageError, ImageResult, ParameterError, ParameterErrorKind, UnsupportedError, UnsupportedErrorKind};
use crate::image::{ImageEncoder, ImageFormat};
use crate::math::utils::clamp;

use super::entropy::build_huff_lut;
use super::transform;

// Markers
// Baseline DCT
static SOF0: u8 = 0xC0;
// Huffman Tables
static DHT: u8 = 0xC4;
// Start of Image (standalone)
static SOI: u8 = 0xD8;
// End of image (standalone)
static EOI: u8 = 0xD9;
// Start of Scan
static SOS: u8 = 0xDA;
// Quantization Tables
static DQT: u8 = 0xDB;
// Application segments start and end
static APP0: u8 = 0xE0;

// section K.1
// table K.1
#[rustfmt::skip]
static STD_LUMA_QTABLE: [u8; 64] = [
    16, 11, 10, 16,  24,  40,  51,  61,
    12, 12, 14, 19,  26,  58,  60,  55,
    14, 13, 16, 24,  40,  57,  69,  56,
    14, 17, 22, 29,  51,  87,  80,  62,
    18, 22, 37, 56,  68, 109, 103,  77,
    24, 35, 55, 64,  81, 104, 113,  92,
    49, 64, 78, 87, 103, 121, 120, 101,
    72, 92, 95, 98, 112, 100, 103,  99,
];

// table K.2
#[rustfmt::skip]
static STD_CHROMA_QTABLE: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99,
    18, 21, 26, 66, 99, 99, 99, 99,
    24, 26, 56, 99, 99, 99, 99, 99,
    47, 66, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
    99, 99, 99, 99, 99, 99, 99, 99,
];

// section K.3
// Code lengths and values for table K.3
static STD_LUMA_DC_CODE_LENGTHS: [u8; 16] = [
    0x00, 0x01, 0x05, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00,
];

static STD_LUMA_DC_VALUES: [u8; 12] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
];

// Code lengths and values for table K.4
static STD_CHROMA_DC_CODE_LENGTHS: [u8; 16] = [
    0x00, 0x03, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x01, 0x00, 0x00, 0x00, 0x00, 0x00,
];

static STD_CHROMA_DC_VALUES: [u8; 12] = [
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0A, 0x0B,
];

// Code lengths and values for table k.5
static STD_LUMA_AC_CODE_LENGTHS: [u8; 16] = [
    0x00, 0x02, 0x01, 0x03, 0x03, 0x02, 0x04, 0x03, 0x05, 0x05, 0x04, 0x04, 0x00, 0x00, 0x01, 0x7D,
];

static STD_LUMA_AC_VALUES: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xA1, 0x08, 0x23, 0x42, 0xB1, 0xC1, 0x15, 0x52, 0xD1, 0xF0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0A, 0x16, 0x17, 0x18, 0x19, 0x1A, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2A, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5, 0xA6, 0xA7,
    0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3, 0xC4, 0xC5,
    0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA, 0xE1, 0xE2,
    0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF1, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

// Code lengths and values for table k.6
static STD_CHROMA_AC_CODE_LENGTHS: [u8; 16] = [
    0x00, 0x02, 0x01, 0x02, 0x04, 0x04, 0x03, 0x04, 0x07, 0x05, 0x04, 0x04, 0x00, 0x01, 0x02, 0x77,
];
static STD_CHROMA_AC_VALUES: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xA1, 0xB1, 0xC1, 0x09, 0x23, 0x33, 0x52, 0xF0,
    0x15, 0x62, 0x72, 0xD1, 0x0A, 0x16, 0x24, 0x34, 0xE1, 0x25, 0xF1, 0x17, 0x18, 0x19, 0x1A, 0x26,
    0x27, 0x28, 0x29, 0x2A, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3A, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4A, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5A, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6A, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7A, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8A, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9A, 0xA2, 0xA3, 0xA4, 0xA5,
    0xA6, 0xA7, 0xA8, 0xA9, 0xAA, 0xB2, 0xB3, 0xB4, 0xB5, 0xB6, 0xB7, 0xB8, 0xB9, 0xBA, 0xC2, 0xC3,
    0xC4, 0xC5, 0xC6, 0xC7, 0xC8, 0xC9, 0xCA, 0xD2, 0xD3, 0xD4, 0xD5, 0xD6, 0xD7, 0xD8, 0xD9, 0xDA,
    0xE2, 0xE3, 0xE4, 0xE5, 0xE6, 0xE7, 0xE8, 0xE9, 0xEA, 0xF2, 0xF3, 0xF4, 0xF5, 0xF6, 0xF7, 0xF8,
    0xF9, 0xFA,
];

static DCCLASS: u8 = 0;
static ACCLASS: u8 = 1;

static LUMADESTINATION: u8 = 0;
static CHROMADESTINATION: u8 = 1;

static LUMAID: u8 = 1;
static CHROMABLUEID: u8 = 2;
static CHROMAREDID: u8 = 3;

/// The permutation of dct coefficients.
#[rustfmt::skip]
static UNZIGZAG: [u8; 64] = [
     0,  1,  8, 16,  9,  2,  3, 10,
    17, 24, 32, 25, 18, 11,  4,  5,
    12, 19, 26, 33, 40, 48, 41, 34,
    27, 20, 13,  6,  7, 14, 21, 28,
    35, 42, 49, 56, 57, 50, 43, 36,
    29, 22, 15, 23, 30, 37, 44, 51,
    58, 59, 52, 45, 38, 31, 39, 46,
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// A representation of a JPEG component
#[derive(Copy, Clone)]
struct Component {
    /// The Component's identifier
    id: u8,

    /// Horizontal sampling factor
    h: u8,

    /// Vertical sampling factor
    v: u8,

    /// The quantization table selector
    tq: u8,

    /// Index to the Huffman DC Table
    dc_table: u8,

    /// Index to the AC Huffman Table
    ac_table: u8,

    /// The dc prediction of the component
    _dc_pred: i32,
}

pub(crate) struct BitWriter<'a, W: 'a> {
    w: &'a mut W,
    accumulator: u32,
    nbits: u8,
}

impl<'a, W: Write + 'a> BitWriter<'a, W> {
    fn new(w: &'a mut W) -> Self {
        BitWriter {
            w,
            accumulator: 0,
            nbits: 0,
        }
    }

    fn write_bits(&mut self, bits: u16, size: u8) -> io::Result<()> {
        if size == 0 {
            return Ok(());
        }

        self.nbits += size;
        self.accumulator |= u32::from(bits) << (32 - self.nbits) as usize;

        while self.nbits >= 8 {
            let byte = self.accumulator >> 24;
            self.w.write_all(&[byte as u8])?;

            if byte == 0xFF {
                self.w.write_all(&[0x00])?;
            }

            self.nbits -= 8;
            self.accumulator <<= 8;
        }

        Ok(())
    }

    fn pad_byte(&mut self) -> io::Result<()> {
        self.write_bits(0x7F, 7)
    }

    fn huffman_encode(&mut self, val: u8, table: &[(u8, u16)]) -> io::Result<()> {
        let (size, code) = table[val as usize];

        if size > 16 {
            panic!("bad huffman value");
        }

        self.write_bits(code, size)
    }

    fn write_block(
        &mut self,
        block: &[i32],
        prevdc: i32,
        dctable: &[(u8, u16)],
        actable: &[(u8, u16)],
    ) -> io::Result<i32> {
        // Differential DC encoding
        let dcval = block[0];
        let diff = dcval - prevdc;
        let (size, value) = encode_coefficient(diff);

        self.huffman_encode(size, dctable)?;
        self.write_bits(value, size)?;

        // Figure F.2
        let mut zero_run = 0;

        for k in 1usize..=63 {
            if block[UNZIGZAG[k] as usize] == 0 {
                zero_run += 1;
            } else {
                while zero_run > 15 {
                    self.huffman_encode(0xF0, actable)?;
                    zero_run -= 16;
                }

                let (size, value) = encode_coefficient(block[UNZIGZAG[k] as usize]);
                let symbol = (zero_run << 4) | size;

                self.huffman_encode(symbol, actable)?;
                self.write_bits(value, size)?;

                zero_run = 0;

                if k == 63 {
                    break;
                }
            }
        }

        if block[UNZIGZAG[63] as usize] == 0 {
            self.huffman_encode(0x00, actable)?;
        }

        Ok(dcval)
    }
    
    fn write_segment(&mut self, marker: u8, data: Option<&[u8]>) -> io::Result<()> {
        self.w.write_all(&[0xFF, marker])?;

        if let Some(b) = data {
            self.w.write_u16::<BigEndian>(b.len() as u16 + 2)?;
            self.w.write_all(b)?;
        }
        Ok(())
    }
}

/// Represents a unit in which the density of an image is measured
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PixelDensityUnit {
    /// Represents the absence of a unit, the values indicate only a
    /// [pixel aspect ratio](https://en.wikipedia.org/wiki/Pixel_aspect_ratio)
    PixelAspectRatio,

    /// Pixels per inch (2.54 cm)
    Inches,

    /// Pixels per centimeter
    Centimeters,
}

/// Represents the pixel density of an image
///
/// For example, a 300 DPI image is represented by:
///
/// ```rust
/// use image::jpeg::*;
/// let hdpi = PixelDensity::dpi(300);
/// assert_eq!(hdpi, PixelDensity {density: (300,300), unit: PixelDensityUnit::Inches})
/// ```
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PixelDensity {
    /// A couple of values for (Xdensity, Ydensity)
    pub density: (u16, u16),
    /// The unit in which the density is measured
    pub unit: PixelDensityUnit,
}

impl PixelDensity {
    /// Creates the most common pixel density type:
    /// the horizontal and the vertical density are equal,
    /// and measured in pixels per inch.
    pub fn dpi(density: u16) -> Self {
        PixelDensity {
            density: (density, density),
            unit: PixelDensityUnit::Inches,
        }
    }
}

impl Default for PixelDensity {
    /// Returns a pixel density with a pixel aspect ratio of 1
    fn default() -> Self {
        PixelDensity {
            density: (1, 1),
            unit: PixelDensityUnit::PixelAspectRatio,
        }
    }
}

/// The representation of a JPEG encoder
pub struct JPEGEncoder<'a, W: 'a> {
    writer: BitWriter<'a, W>,

    components: Vec<Component>,
    tables: Vec<u8>,

    luma_dctable: Vec<(u8, u16)>,
    luma_actable: Vec<(u8, u16)>,
    chroma_dctable: Vec<(u8, u16)>,
    chroma_actable: Vec<(u8, u16)>,

    pixel_density: PixelDensity,
}

impl<'a, W: Write> JPEGEncoder<'a, W> {
    /// Create a new encoder that writes its output to ```w```
    pub fn new(w: &mut W) -> JPEGEncoder<W> {
        JPEGEncoder::new_with_quality(w, 75)
    }

    /// Create a new encoder that writes its output to ```w```, and has
    /// the quality parameter ```quality``` with a value in the range 1-100
    /// where 1 is the worst and 100 is the best.
    pub fn new_with_quality(w: &mut W, quality: u8) -> JPEGEncoder<W> {
        let ld = build_huff_lut(&STD_LUMA_DC_CODE_LENGTHS, &STD_LUMA_DC_VALUES);
        let la = build_huff_lut(&STD_LUMA_AC_CODE_LENGTHS, &STD_LUMA_AC_VALUES);

        let cd = build_huff_lut(&STD_CHROMA_DC_CODE_LENGTHS, &STD_CHROMA_DC_VALUES);
        let ca = build_huff_lut(&STD_CHROMA_AC_CODE_LENGTHS, &STD_CHROMA_AC_VALUES);

        let components = vec![
            Component {
                id: LUMAID,
                h: 1,
                v: 1,
                tq: LUMADESTINATION,
                dc_table: LUMADESTINATION,
                ac_table: LUMADESTINATION,
                _dc_pred: 0,
            },
            Component {
                id: CHROMABLUEID,
                h: 1,
                v: 1,
                tq: CHROMADESTINATION,
                dc_table: CHROMADESTINATION,
                ac_table: CHROMADESTINATION,
                _dc_pred: 0,
            },
            Component {
                id: CHROMAREDID,
                h: 1,
                v: 1,
                tq: CHROMADESTINATION,
                dc_table: CHROMADESTINATION,
                ac_table: CHROMADESTINATION,
                _dc_pred: 0,
            },
        ];

        // Derive our quantization table scaling value using the libjpeg algorithm
        let scale = u32::from(clamp(quality, 1, 100));
        let scale = if scale < 50 {
            5000 / scale
        } else {
            200 - scale * 2
        };

        let mut tables = Vec::new();
        let scale_value = |&v: &u8| {
            let value = (u32::from(v) * scale + 50) / 100;

            clamp(value, 1, u32::from(u8::max_value())) as u8
        };
        tables.extend(STD_LUMA_QTABLE.iter().map(&scale_value));
        tables.extend(STD_CHROMA_QTABLE.iter().map(&scale_value));

        JPEGEncoder {
            writer: BitWriter::new(w),

            components,
            tables,

            luma_dctable: ld,
            luma_actable: la,
            chroma_dctable: cd,
            chroma_actable: ca,

            pixel_density: PixelDensity::default(),
        }
    }

    /// Set the pixel density of the images the encoder will encode.
    /// If this method is not called, then a default pixel aspect ratio of 1x1 will be applied,
    /// and no DPI information will be stored in the image.
    pub fn set_pixel_density(&mut self, pixel_density: PixelDensity) {
        self.pixel_density = pixel_density;
    }

    /// Encodes the image stored in the raw byte buffer ```image```
    /// that has dimensions ```width``` and ```height```
    /// and ```ColorType``` ```c```
    ///
    /// The Image in encoded with subsampling ratio 4:2:2
    pub fn encode(
        &mut self,
        image: &[u8],
        width: u32,
        height: u32,
        color_type: ColorType,
    ) -> ImageResult<()> {
        match color_type {
            ColorType::L8 => {
                let image: ImageBuffer<Luma<_>, _> = ImageBuffer::from_raw(width, height, image).unwrap();
                self.encode_image(&image)
            },
            ColorType::La8 => {
                let image: ImageBuffer<LumaA<_>, _> = ImageBuffer::from_raw(width, height, image).unwrap();
                self.encode_image(&image)
            },
            ColorType::Rgb8 => {
                let image: ImageBuffer<Rgb<_>, _> = ImageBuffer::from_raw(width, height, image).unwrap();
                self.encode_image(&image)
            },
            ColorType::Rgba8 => {
                let image: ImageBuffer<Rgba<_>, _> = ImageBuffer::from_raw(width, height, image).unwrap();
                self.encode_image(&image)
            },
            ColorType::Bgr8 => {
                let image: ImageBuffer<Bgr<_>, _> = ImageBuffer::from_raw(width, height, image).unwrap();
                self.encode_image(&image)
            },
            ColorType::Bgra8 => {
                let image: ImageBuffer<Bgra<_>, _> = ImageBuffer::from_raw(width, height, image).unwrap();
                self.encode_image(&image)
            },
            _ => {
                return Err(ImageError::Unsupported(
                    UnsupportedError::from_format_and_kind(
                        ImageFormat::Jpeg.into(),
                        UnsupportedErrorKind::Color(color_type.into()),
                    ),
                ))
            },
        }
    }

    /// Encodes the given image.
    ///
    /// As a special feature this does not require the whole image to be present in memory at the
    /// same time such that it may be computed on the fly, which is why this method exists on this
    /// encoder but not on others. Instead the encoder will iterate over 8-by-8 blocks of pixels at
    /// a time, inspecting each pixel exactly once. You can rely on this behaviour when calling
    /// this method.
    ///
    /// The Image in encoded with subsampling ratio 4:2:2
    pub fn encode_image<I: GenericImageView>(
        &mut self,
        image: &I,
    ) -> ImageResult<()> {
        let n = I::Pixel::CHANNEL_COUNT;
        let num_components = if n == 1 || n == 2 { 1 } else { 3 };

        self.writer.write_segment(SOI, None)?;

        let mut buf = Vec::new();

        build_jfif_header(&mut buf, self.pixel_density);
        self.writer.write_segment(APP0, Some(&buf))?;

        build_frame_header(
            &mut buf,
            8,
            // TODO: not idiomatic yet. Should be an EncodingError and mention jpg. Further it
            // should check dimensions prior to writing.
            u16::try_from(image.width()).map_err(|_| {
                ImageError::Parameter(ParameterError::from_kind(
                    ParameterErrorKind::DimensionMismatch,
                ))
            })?,
            u16::try_from(image.height()).map_err(|_| {
                ImageError::Parameter(ParameterError::from_kind(
                    ParameterErrorKind::DimensionMismatch,
                ))
            })?,
            &self.components[..num_components],
        );
        self.writer.write_segment(SOF0, Some(&buf))?;

        assert_eq!(self.tables.len() / 64, 2);
        let numtables = if num_components == 1 { 1 } else { 2 };

        for (i, table) in self.tables.chunks(64).enumerate().take(numtables) {
            build_quantization_segment(&mut buf, 8, i as u8, table);
            self.writer.write_segment(DQT, Some(&buf))?;
        }

        build_huffman_segment(
            &mut buf,
            DCCLASS,
            LUMADESTINATION,
            &STD_LUMA_DC_CODE_LENGTHS,
            &STD_LUMA_DC_VALUES,
        );
        self.writer.write_segment(DHT, Some(&buf))?;

        build_huffman_segment(
            &mut buf,
            ACCLASS,
            LUMADESTINATION,
            &STD_LUMA_AC_CODE_LENGTHS,
            &STD_LUMA_AC_VALUES,
        );
        self.writer.write_segment(DHT, Some(&buf))?;

        if num_components == 3 {
            build_huffman_segment(
                &mut buf,
                DCCLASS,
                CHROMADESTINATION,
                &STD_CHROMA_DC_CODE_LENGTHS,
                &STD_CHROMA_DC_VALUES,
            );
            self.writer.write_segment(DHT, Some(&buf))?;

            build_huffman_segment(
                &mut buf,
                ACCLASS,
                CHROMADESTINATION,
                &STD_CHROMA_AC_CODE_LENGTHS,
                &STD_CHROMA_AC_VALUES,
            );
            self.writer.write_segment(DHT, Some(&buf))?;
        }

        build_scan_header(&mut buf, &self.components[..num_components]);
        self.writer.write_segment(SOS, Some(&buf))?;


        if I::Pixel::COLOR_TYPE.has_color() {
            self.encode_rgb(image)
        } else {
            self.encode_gray(image)
        }?;

        self.writer.pad_byte()?;
        self.writer.write_segment(EOI, None)?;
        Ok(())
    }

    fn encode_gray<I: GenericImageView>(
        &mut self,
        image: &I,
    ) -> io::Result<()> {
        let mut yblock = [0u8; 64];
        let mut y_dcprev = 0;
        let mut dct_yblock = [0i32; 64];

        for y in range_step(0, image.height(), 8) {
            for x in range_step(0, image.width(), 8) {
                copy_blocks_gray(image, x, y, &mut yblock);

                // Level shift and fdct
                // Coeffs are scaled by 8
                transform::fdct(&yblock, &mut dct_yblock);

                // Quantization
                for (i, dct) in dct_yblock.iter_mut().enumerate().take(64) {
                    *dct = ((*dct / 8) as f32 / f32::from(self.tables[i])).round() as i32;
                }

                let la = &*self.luma_actable;
                let ld = &*self.luma_dctable;

                y_dcprev = self.writer.write_block(&dct_yblock, y_dcprev, ld, la)?;
            }
        }

        Ok(())
    }

    fn encode_rgb<I: GenericImageView>(
        &mut self,
        image: &I,
    ) -> io::Result<()> {
        let mut y_dcprev = 0;
        let mut cb_dcprev = 0;
        let mut cr_dcprev = 0;

        let mut dct_yblock = [0i32; 64];
        let mut dct_cb_block = [0i32; 64];
        let mut dct_cr_block = [0i32; 64];

        let mut yblock = [0u8; 64];
        let mut cb_block = [0u8; 64];
        let mut cr_block = [0u8; 64];

        for y in range_step(0, image.height(), 8) {
            for x in range_step(0, image.width(), 8) {
                // RGB -> YCbCr
                copy_blocks_ycbcr(
                    image,
                    x,
                    y,
                    &mut yblock,
                    &mut cb_block,
                    &mut cr_block,
                );

                // Level shift and fdct
                // Coeffs are scaled by 8
                transform::fdct(&yblock, &mut dct_yblock);
                transform::fdct(&cb_block, &mut dct_cb_block);
                transform::fdct(&cr_block, &mut dct_cr_block);

                // Quantization
                for i in 0usize..64 {
                    dct_yblock[i] =
                        ((dct_yblock[i] / 8) as f32 / f32::from(self.tables[i])).round() as i32;
                    dct_cb_block[i] = ((dct_cb_block[i] / 8) as f32
                        / f32::from(self.tables[64+i]))
                        .round() as i32;
                    dct_cr_block[i] = ((dct_cr_block[i] / 8) as f32
                        / f32::from(self.tables[64+i]))
                        .round() as i32;
                }

                let la = &*self.luma_actable;
                let ld = &*self.luma_dctable;
                let cd = &*self.chroma_dctable;
                let ca = &*self.chroma_actable;

                y_dcprev = self.writer.write_block(&dct_yblock, y_dcprev, ld, la)?;
                cb_dcprev = self.writer.write_block(&dct_cb_block, cb_dcprev, cd, ca)?;
                cr_dcprev = self.writer.write_block(&dct_cr_block, cr_dcprev, cd, ca)?;
            }
        }

        Ok(())
    }
}

impl<'a, W: Write> ImageEncoder for JPEGEncoder<'a, W> {
    fn write_image(
        mut self,
        buf: &[u8],
        width: u32,
        height: u32,
        color_type: ColorType,
    ) -> ImageResult<()> {
        self.encode(buf, width, height, color_type)
    }
}

fn build_jfif_header(m: &mut Vec<u8>, density: PixelDensity) {
    m.clear();

    // TODO: More idiomatic would be extend_from_slice, to_be_bytes
    let _ = write!(m, "JFIF");
    let _ = m.write_all(&[0]);
    let _ = m.write_all(&[0x01]);
    let _ = m.write_all(&[0x02]);
    let _ = m.write_all(&[match density.unit {
        PixelDensityUnit::PixelAspectRatio => 0x00,
        PixelDensityUnit::Inches => 0x01,
        PixelDensityUnit::Centimeters => 0x02,
    }]);
    let _ = m.write_u16::<BigEndian>(density.density.0);
    let _ = m.write_u16::<BigEndian>(density.density.1);
    let _ = m.write_all(&[0]);
    let _ = m.write_all(&[0]);
}

fn build_frame_header(
    m: &mut Vec<u8>,
    precision: u8,
    width: u16,
    height: u16,
    components: &[Component],
) {
    m.clear();

    // TODO: More idiomatic would be extend_from_slice, to_be_bytes
    let _ = m.write_all(&[precision]);
    let _ = m.write_u16::<BigEndian>(height);
    let _ = m.write_u16::<BigEndian>(width);
    let _ = m.write_all(&[components.len() as u8]);

    for &comp in components.iter() {
        let _ = m.write_all(&[comp.id]);
        let hv = (comp.h << 4) | comp.v;
        let _ = m.write_all(&[hv]);
        let _ = m.write_all(&[comp.tq]);
    }
}

fn build_scan_header(m: &mut Vec<u8>, components: &[Component]) {
    m.clear();

    // TODO: More idiomatic would be extend_from_slice, to_be_bytes
    let _ = m.write_all(&[components.len() as u8]);

    for &comp in components.iter() {
        let _ = m.write_all(&[comp.id]);
        let tables = (comp.dc_table << 4) | comp.ac_table;
        let _ = m.write_all(&[tables]);
    }

    // spectral start and end, approx. high and low
    let _ = m.write_all(&[0]);
    let _ = m.write_all(&[63]);
    let _ = m.write_all(&[0]);
}

fn build_huffman_segment(
    m: &mut Vec<u8>,
    class: u8,
    destination: u8,
    numcodes: &[u8],
    values: &[u8],
) {
    m.clear();

    // TODO: More idiomatic would be pub, extend_from_slice
    let tcth = (class << 4) | destination;
    let _ = m.write_u8(tcth);

    assert_eq!(numcodes.len(), 16);

    let _ = m.write_all(numcodes);

    let mut sum = 0usize;

    for &i in numcodes.iter() {
        sum += i as usize;
    }

    assert_eq!(sum, values.len());

    let _ = m.write_all(values);
}

fn build_quantization_segment(m: &mut Vec<u8>, precision: u8, identifier: u8, qtable: &[u8]) {
    assert_eq!(qtable.len() % 64, 0);
    m.clear();

    // TODO: More idiomatic would be pub, extend_from_slice
    let p = if precision == 8 { 0 } else { 1 };

    let pqtq = (p << 4) | identifier;
    let _ = m.write_u8(pqtq);

    for &i in &UNZIGZAG[..] {
        let _ = m.write_u8(qtable[i as usize]);
    }
}

fn encode_coefficient(coefficient: i32) -> (u8, u16) {
    let mut magnitude = coefficient.abs() as u16;
    let mut num_bits = 0u8;

    while magnitude > 0 {
        magnitude >>= 1;
        num_bits += 1;
    }

    let mask = (1 << num_bits as usize) - 1;

    let val = if coefficient < 0 {
        (coefficient - 1) as u16 & mask
    } else {
        coefficient as u16 & mask
    };

    (num_bits, val)
}

#[inline]
fn rgb_to_ycbcr<P: Pixel>(pixel: P) -> (u8, u8, u8) {
    use num_traits::{cast::ToPrimitive, bounds::Bounded};
    let [r, g, b] = pixel.to_rgb().0;
    let max: f32 = P::Subpixel::max_value().to_f32().unwrap();
    let r: f32 = r.to_f32().unwrap();
    let g: f32 = g.to_f32().unwrap();
    let b: f32 = b.to_f32().unwrap();

    // Coefficients from JPEG File Interchange Format (Version 1.02), multiplied for 255 maximum.
    let y = 76.245 / max * r + 149.685 / max * g + 29.07 / max * b;
    let cb = -43.0185 / max * r - 84.4815 / max * g + 127.5 / max * b + 128.;
    let cr = 127.5 / max * r - 106.7685 / max * g - 20.7315 / max * b + 128.;

    (y as u8, cb as u8, cr as u8)
}


/// Returns the pixel at (x,y) if (x,y) is in the image,
/// otherwise the closest pixel in the image
#[inline]
fn pixel_at_or_near<I: GenericImageView>(source: &I, x: u32, y: u32) -> I::Pixel {
    if source.in_bounds(x, y) {
        source.get_pixel(x, y)
    } else {
        source.get_pixel(
            x.min(source.width() - 1),
            y.min(source.height() - 1),
        )
    }
}

fn copy_blocks_ycbcr<I: GenericImageView>(
    source: &I,
    x0: u32,
    y0: u32,
    yb: &mut [u8; 64],
    cbb: &mut [u8; 64],
    crb: &mut [u8; 64],
) {
    for y in 0..8 {
        for x in 0..8 {
            let pixel = pixel_at_or_near(source, x + x0, y + y0);
            let (yc, cb, cr) = rgb_to_ycbcr(pixel);

            yb[(y * 8 + x) as usize] = yc;
            cbb[(y * 8 + x) as usize] = cb;
            crb[(y * 8 + x) as usize] = cr;
        }
    }
}

fn copy_blocks_gray<I: GenericImageView>(
    source: &I,
    x0: u32,
    y0: u32,
    gb: &mut [u8; 64],
) {
    use num_traits::cast::ToPrimitive;
    for y in 0..8 {
        for x in 0..8 {
            let pixel = pixel_at_or_near(source, x0 + x, y0 + y);
            let [luma] = pixel.to_luma().0;
            gb[(y * 8 + x) as usize] = luma.to_u8().unwrap();
        }
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use crate::{Bgra, ImageBuffer, ImageEncoder, ImageError};
    use crate::color::ColorType;
    use crate::error::ParameterErrorKind::DimensionMismatch;
    use crate::image::ImageDecoder;

    use super::{build_jfif_header, JPEGEncoder, PixelDensity};
    use super::super::JpegDecoder;

    fn decode(encoded: &[u8]) -> Vec<u8> {
        let decoder = JpegDecoder::new(Cursor::new(encoded))
            .expect("Could not decode image");

        let mut decoded = vec![0; decoder.total_bytes() as usize];
        decoder.read_image(&mut decoded).expect("Could not decode image");
        decoded
    }

    #[test]
    fn roundtrip_sanity_check() {
        // create a 1x1 8-bit image buffer containing a single red pixel
        let img = [255u8, 0, 0];

        // encode it into a memory buffer
        let mut encoded_img = Vec::new();
        {
            let encoder = JPEGEncoder::new_with_quality(&mut encoded_img, 100);
            encoder
                .write_image(&img, 1, 1, ColorType::Rgb8)
                .expect("Could not encode image");
        }

        // decode it from the memory buffer
        {
            let decoded = decode(&encoded_img);
            // note that, even with the encode quality set to 100, we do not get the same image
            // back. Therefore, we're going to assert that it's at least red-ish:
            assert_eq!(3, decoded.len());
            assert!(decoded[0] > 0x80);
            assert!(decoded[1] < 0x80);
            assert!(decoded[2] < 0x80);
        }
    }

    #[test]
    fn grayscale_roundtrip_sanity_check() {
        // create a 2x2 8-bit image buffer containing a white diagonal
        let img = [255u8, 0, 0, 255];

        // encode it into a memory buffer
        let mut encoded_img = Vec::new();
        {
            let encoder = JPEGEncoder::new_with_quality(&mut encoded_img, 100);
            encoder
                .write_image(&img[..], 2, 2, ColorType::L8)
                .expect("Could not encode image");
        }

        // decode it from the memory buffer
        {
            let decoded = decode(&encoded_img);
            // note that, even with the encode quality set to 100, we do not get the same image
            // back. Therefore, we're going to assert that the diagonal is at least white-ish:
            assert_eq!(4, decoded.len());
            assert!(decoded[0] > 0x80);
            assert!(decoded[1] < 0x80);
            assert!(decoded[2] < 0x80);
            assert!(decoded[3] > 0x80);
        }
    }

    #[test]
    fn jfif_header_density_check() {
        let mut buffer = Vec::new();
        build_jfif_header(&mut buffer, PixelDensity::dpi(300));
        assert_eq!(buffer, vec![
                b'J', b'F', b'I', b'F',
                0, 1, 2, // JFIF version 1.2
                1, // density is in dpi
                300u16.to_be_bytes()[0], 300u16.to_be_bytes()[1],
                300u16.to_be_bytes()[0], 300u16.to_be_bytes()[1],
                0, 0, // No thumbnail
            ]
        );
    }


    #[test]
    fn test_image_too_large() {
        // JPEG cannot encode images larger than 65,535×65,535
        // create a 65,536×1 8-bit black image buffer
        let img = [0; 65_536];
        // Try to encode an image that is too large
        let mut encoded = Vec::new();
        let encoder = JPEGEncoder::new_with_quality(&mut encoded, 100);
        let result = encoder.write_image(&img, 65_536, 1, ColorType::L8);
        match result {
            Err(ImageError::Parameter(err)) => {
                assert_eq!(err.kind(), DimensionMismatch)
            }
            other => {
                assert!(false, "Encoding an image that is too large should return a DimensionError \
                                it returned {:?} instead", other)
            }
        }
    }

    #[test]
    fn test_bgra16() {
        // Test encoding an RGBA 16-bit image.
        // Jpeg is RGB 8-bit, so the conversion should be done on the fly
        let mut encoded = Vec::new();
        let max = std::u16::MAX;
        let image: ImageBuffer<Bgra<u16>, _> = ImageBuffer::from_raw(
            1, 1, vec![0, max / 2, max, max]).unwrap();
        let mut encoder = JPEGEncoder::new_with_quality(&mut encoded, 100);
        encoder.encode_image(&image).unwrap();
        let decoded = decode(&encoded);
        assert!(decoded[0] > 200, "bad red channel in {:?}", &decoded);
        assert!(100 < decoded[1] && decoded[1] < 150, "bad green channel in {:?}", &decoded);
        assert!(decoded[2] < 50, "bad blue channel in {:?}", &decoded);
    }
}
