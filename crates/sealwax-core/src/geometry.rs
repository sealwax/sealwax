/// A rectangle in PDF user space, normalized so that `x1 <= x2` and `y1 <= y2`.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct Rect {
    /// Left edge.
    pub x1: f64,
    /// Bottom edge.
    pub y1: f64,
    /// Right edge.
    pub x2: f64,
    /// Top edge.
    pub y2: f64,
}

impl Rect {
    /// A rectangle from two corners given in any order.
    pub fn from_corners(ax: f64, ay: f64, bx: f64, by: f64) -> Self {
        Self {
            x1: ax.min(bx),
            y1: ay.min(by),
            x2: ax.max(bx),
            y2: ay.max(by),
        }
    }

    /// The part of `self` inside `other`; an empty rectangle at the overlap's corner when they
    /// do not overlap.
    pub fn intersect(&self, other: &Self) -> Self {
        let x1 = self.x1.max(other.x1);
        let y1 = self.y1.max(other.y1);
        Self {
            x1,
            y1,
            x2: self.x2.min(other.x2).max(x1),
            y2: self.y2.min(other.y2).max(y1),
        }
    }

    /// Width.
    pub fn width(&self) -> f64 {
        self.x2 - self.x1
    }

    /// Height.
    pub fn height(&self) -> f64 {
        self.y2 - self.y1
    }

    /// The corners as `(x1, y1, x2, y2)`.
    pub fn to_tuple(&self) -> (f64, f64, f64, f64) {
        (self.x1, self.y1, self.x2, self.y2)
    }
}

/// Display rotation of a page, clockwise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub enum Rotation {
    /// 0 degrees.
    None,
    /// 90 degrees.
    Quarter,
    /// 180 degrees.
    Half,
    /// 270 degrees.
    ThreeQuarters,
}

impl Rotation {
    /// The rotation in degrees: 0, 90, 180 or 270.
    pub fn degrees(self) -> u16 {
        match self {
            Rotation::None => 0,
            Rotation::Quarter => 90,
            Rotation::Half => 180,
            Rotation::ThreeQuarters => 270,
        }
    }

    /// This rotation turned `quarter_turns` more quarter turns clockwise (counterclockwise when
    /// negative), back to none after a full turn.
    #[must_use]
    pub fn turned(self, quarter_turns: i32) -> Self {
        let quarters = i32::from(self.degrees() / 90);
        match (quarters + quarter_turns).rem_euclid(4) {
            1 => Rotation::Quarter,
            2 => Rotation::Half,
            3 => Rotation::ThreeQuarters,
            _ => Rotation::None,
        }
    }
}

/// The geometry a page declares, as stored in the PDF: user-space points, bottom-left origin.
#[derive(Debug, Clone, Copy, PartialEq)]
#[cfg_attr(feature = "serde", derive(serde::Serialize))]
pub struct PageGeometry {
    /// 1-based page number.
    pub page_number: usize,
    /// Media box, inherited from the page tree when the page has none; US Letter when no level
    /// declares one, as PDFium and other readers assume.
    pub media_box: Rect,
    /// Crop box, inherited, defaulting to the media box, and clipped to the media box
    /// (ISO 32000-1, 14.11.2). The whole media box when the clipped crop box has no area.
    pub crop_box: Rect,
    /// Display rotation.
    pub rotation: Rotation,
    /// Size of one user-space unit in multiples of 1/72 inch; 1.0 when the page declares none.
    pub user_unit: f64,
}
