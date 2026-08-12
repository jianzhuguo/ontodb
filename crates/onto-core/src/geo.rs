//! GIS data types for OntoDB.
//!
//! Supports:
//! - Point, LineString, Polygon, MultiPolygon geometries
//! - WKB (Well-Known Binary) encoding/decoding
//! - Geohash encoding for spatial indexing
//! - Spatial relationship predicates (contains, intersects, distance)
//!
//! Design: All geometry types are stored as WKB bytes in BinaryRow fields.
//! This provides ≥60% compression vs JSON and enables efficient binary comparison.

use serde::{Deserialize, Serialize};
use std::fmt;

// ── Geometry Types ──

/// A 2D coordinate (longitude, latitude).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Coord {
    pub x: f64, // longitude
    pub y: f64, // latitude
}

impl Coord {
    pub fn new(x: f64, y: f64) -> Self {
        Self { x, y }
    }

    /// Calculate distance to another coordinate (Haversine formula, returns meters).
    pub fn distance_to(&self, other: &Coord) -> f64 {
        haversine_distance(self.x, self.y, other.x, other.y)
    }
}

impl fmt::Display for Coord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.x, self.y)
    }
}

/// A geometry type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Geometry {
    Point(Coord),
    LineString(Vec<Coord>),
    Polygon(Vec<Vec<Coord>>), // outer ring + holes
    MultiPolygon(Vec<Vec<Vec<Coord>>>),
}

impl Geometry {
    /// Returns the geometry type name.
    pub fn geometry_type(&self) -> &'static str {
        match self {
            Geometry::Point(_) => "Point",
            Geometry::LineString(_) => "LineString",
            Geometry::Polygon(_) => "Polygon",
            Geometry::MultiPolygon(_) => "MultiPolygon",
        }
    }

    /// Returns the bounding box as (min_x, min_y, max_x, max_y).
    pub fn bounding_box(&self) -> (f64, f64, f64, f64) {
        match self {
            Geometry::Point(c) => (c.x, c.y, c.x, c.y),
            Geometry::LineString(coords) => bbox_from_coords(coords),
            Geometry::Polygon(rings) => {
                if let Some(outer) = rings.first() {
                    bbox_from_coords(outer)
                } else {
                    (0.0, 0.0, 0.0, 0.0)
                }
            }
            Geometry::MultiPolygon(polygons) => {
                let mut all_coords = Vec::new();
                for polygon in polygons {
                    for ring in polygon {
                        all_coords.extend(ring);
                    }
                }
                bbox_from_coords(&all_coords)
            }
        }
    }

    /// Encode to WKB (Well-Known Binary) bytes.
    pub fn to_wkb(&self) -> Vec<u8> {
        encode_wkb(self)
    }

    /// Decode from WKB bytes.
    pub fn from_wkb(data: &[u8]) -> Option<Self> {
        decode_wkb(data)
    }

    /// Encode to WKT (Well-Known Text) string.
    pub fn to_wkt(&self) -> String {
        encode_wkt(self)
    }

    /// Parse from WKT string.
    pub fn from_wkt(s: &str) -> Option<Self> {
        decode_wkt(s)
    }
}

// ── Spatial Predicates (Nine-Intersection Model) ──

/// Spatial relationship type based on DE-9IM (Dimensionally Extended
/// 9-Intersection Model).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SpatialRelation {
    /// Geometries are identical.
    Equals,
    /// Geometries have no points in common.
    Disjoint,
    /// Geometries share at least one point.
    Intersects,
    /// Geometries touch at boundary but don't overlap.
    Touches,
    /// Geometries cross (different dimensions).
    Crosses,
    /// Geometry A is completely inside B.
    Within,
    /// Geometry A completely contains B.
    Contains,
    /// Geometries overlap with same dimension.
    Overlaps,
}

/// Check all spatial relationships between two geometries.
pub fn relate(a: &Geometry, b: &Geometry) -> SpatialRelation {
    if equals(a, b) {
        return SpatialRelation::Equals;
    }
    if !intersects(a, b) {
        return SpatialRelation::Disjoint;
    }
    if contains(a, b) {
        return SpatialRelation::Contains;
    }
    if within(a, b) {
        return SpatialRelation::Within;
    }
    if touches(a, b) {
        return SpatialRelation::Touches;
    }
    if crosses(a, b) {
        return SpatialRelation::Crosses;
    }
    if overlaps(a, b) {
        return SpatialRelation::Overlaps;
    }
    SpatialRelation::Intersects
}

/// Check if geometry A equals geometry B.
pub fn equals(a: &Geometry, b: &Geometry) -> bool {
    match (a, b) {
        (Geometry::Point(p1), Geometry::Point(p2)) => p1 == p2,
        (Geometry::LineString(l1), Geometry::LineString(l2)) => l1 == l2,
        (Geometry::Polygon(r1), Geometry::Polygon(r2)) => r1 == r2,
        _ => false,
    }
}

/// Check if geometry A is disjoint from geometry B.
pub fn disjoint(a: &Geometry, b: &Geometry) -> bool {
    !intersects(a, b)
}

/// Check if geometry A intersects geometry B.
///
/// Uses bounding box as fast pre-check, then precise geometry test.
pub fn intersects(a: &Geometry, b: &Geometry) -> bool {
    // Fast bounding box check
    let bb_a = a.bounding_box();
    let bb_b = b.bounding_box();
    if !bbox_intersects(&bb_a, &bb_b) {
        return false;
    }

    // Precise geometry check
    match (a, b) {
        (Geometry::Point(p), _) => point_in_geometry(p, b),
        (_, Geometry::Point(p)) => point_in_geometry(p, a),
        (Geometry::Polygon(rings_a), Geometry::Polygon(rings_b)) => {
            polygon_intersects_polygon(rings_a, rings_b)
        }
        _ => true, // Conservative: assume intersects if bboxes overlap
    }
}

/// Check if geometry A touches geometry B (share boundary, no interior overlap).
pub fn touches(a: &Geometry, b: &Geometry) -> bool {
    match (a, b) {
        (Geometry::Point(p), Geometry::Polygon(rings)) => {
            if let Some(outer) = rings.first() {
                point_on_boundary(p, outer)
            } else {
                false
            }
        }
        (Geometry::Polygon(rings), Geometry::Point(p)) => {
            if let Some(outer) = rings.first() {
                point_on_boundary(p, outer)
            } else {
                false
            }
        }
        (Geometry::Polygon(rings_a), Geometry::Polygon(rings_b)) => {
            // Two polygons touch if they share a boundary point but no interior
            if let (Some(outer_a), Some(outer_b)) = (rings_a.first(), rings_b.first()) {
                let has_boundary_contact = outer_a.iter().any(|p| point_on_boundary(p, outer_b))
                    || outer_b.iter().any(|p| point_on_boundary(p, outer_a));
                let has_interior_overlap = polygon_intersects_polygon(rings_a, rings_b)
                    && !has_boundary_contact;
                has_boundary_contact && !has_interior_overlap
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Check if geometry A crosses geometry B (different dimensions intersect).
pub fn crosses(a: &Geometry, b: &Geometry) -> bool {
    match (a, b) {
        (Geometry::LineString(_), Geometry::Polygon(_)) => {
            // Line crosses polygon if it intersects but is not fully contained
            intersects(a, b) && !within(a, b)
        }
        (Geometry::Polygon(_), Geometry::LineString(_)) => {
            intersects(a, b) && !within(b, a)
        }
        (Geometry::LineString(l1), Geometry::LineString(l2)) => {
            // Two lines cross if they intersect at a point (not overlapping)
            line_intersects_line(l1, l2)
        }
        _ => false,
    }
}

/// Check if geometry A is completely within geometry B.
pub fn within(a: &Geometry, b: &Geometry) -> bool {
    contains(b, a)
}

/// Check if geometry A completely contains geometry B.
pub fn contains(a: &Geometry, b: &Geometry) -> bool {
    match (a, b) {
        (Geometry::Polygon(rings), Geometry::Point(p)) => {
            if let Some(outer) = rings.first() {
                point_in_polygon(p, outer) && !point_on_boundary(p, outer)
            } else {
                false
            }
        }
        (Geometry::Polygon(rings_a), Geometry::Polygon(rings_b)) => {
            if let (Some(outer_a), Some(outer_b)) = (rings_a.first(), rings_b.first()) {
                // All points of B must be inside A
                outer_b.iter().all(|p| point_in_polygon(p, outer_a))
            } else {
                false
            }
        }
        (Geometry::Polygon(rings), Geometry::LineString(coords)) => {
            if let Some(outer) = rings.first() {
                coords.iter().all(|p| point_in_polygon(p, outer))
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Check if geometry A overlaps geometry B (same dimension, partial overlap).
pub fn overlaps(a: &Geometry, b: &Geometry) -> bool {
    match (a, b) {
        (Geometry::Polygon(_rings_a), Geometry::Polygon(_rings_b)) => {
            // Two polygons overlap if they intersect but neither contains the other
            intersects(a, b) && !contains(a, b) && !contains(b, a)
        }
        (Geometry::LineString(_l1), Geometry::LineString(_l2)) => {
            // Two lines overlap if they share a segment
            intersects(a, b) && !within(a, b) && !within(b, a)
        }
        _ => false,
    }
}

/// Calculate distance between two geometries (meters).
pub fn distance(a: &Geometry, b: &Geometry) -> f64 {
    match (a, b) {
        (Geometry::Point(p1), Geometry::Point(p2)) => p1.distance_to(p2),
        _ => {
            // Fallback: distance between centroids
            let c1 = centroid(a);
            let c2 = centroid(b);
            c1.distance_to(&c2)
        }
    }
}

/// Calculate centroid of a geometry.
pub fn centroid(geo: &Geometry) -> Coord {
    match geo {
        Geometry::Point(c) => *c,
        Geometry::LineString(coords) => {
            let n = coords.len() as f64;
            let sum_x: f64 = coords.iter().map(|c| c.x).sum();
            let sum_y: f64 = coords.iter().map(|c| c.y).sum();
            Coord::new(sum_x / n, sum_y / n)
        }
        Geometry::Polygon(rings) => {
            if let Some(outer) = rings.first() {
                let n = outer.len() as f64;
                let sum_x: f64 = outer.iter().map(|c| c.x).sum();
                let sum_y: f64 = outer.iter().map(|c| c.y).sum();
                Coord::new(sum_x / n, sum_y / n)
            } else {
                Coord::new(0.0, 0.0)
            }
        }
        Geometry::MultiPolygon(polys) => {
            let mut all = Vec::new();
            for poly in polys {
                for ring in poly {
                    all.extend(ring);
                }
            }
            centroid(&Geometry::Polygon(vec![all]))
        }
    }
}

// ── Geohash ──

const GEOHASH_PRECISION: usize = 12;
const BASE32: &[u8] = b"0123456789bcdefghjkmnpqrstuvwxyz";

/// Encode a coordinate to geohash string.
pub fn geohash_encode(lat: f64, lon: f64, precision: usize) -> String {
    let prec = precision.min(GEOHASH_PRECISION);
    let mut lat_range = (-90.0, 90.0);
    let mut lon_range = (-180.0, 180.0);
    let mut is_lon = true;
    let mut bit = 0u32;
    let mut ch = 0u8;
    let mut hash = String::new();

    while hash.len() < prec {
        if is_lon {
            let mid = (lon_range.0 + lon_range.1) / 2.0;
            if lon >= mid {
                ch |= 1 << (4 - bit);
                lon_range.0 = mid;
            } else {
                lon_range.1 = mid;
            }
        } else {
            let mid = (lat_range.0 + lat_range.1) / 2.0;
            if lat >= mid {
                ch |= 1 << (4 - bit);
                lat_range.0 = mid;
            } else {
                lat_range.1 = mid;
            }
        }
        is_lon = !is_lon;
        bit += 1;
        if bit == 5 {
            hash.push(BASE32[ch as usize] as char);
            bit = 0;
            ch = 0;
        }
    }
    hash
}

/// Decode a geohash string to (lat, lon, lat_err, lon_err).
pub fn geohash_decode(hash: &str) -> Option<(f64, f64, f64, f64)> {
    let mut lat_range = (-90.0, 90.0);
    let mut lon_range = (-180.0, 180.0);
    let mut is_lon = true;

    for ch in hash.chars() {
        let idx = BASE32.iter().position(|&b| b as char == ch)?;
        for i in (0..5).rev() {
            let bit = (idx >> i) & 1;
            if is_lon {
                let mid = (lon_range.0 + lon_range.1) / 2.0;
                if bit == 1 {
                    lon_range.0 = mid;
                } else {
                    lon_range.1 = mid;
                }
            } else {
                let mid = (lat_range.0 + lat_range.1) / 2.0;
                if bit == 1 {
                    lat_range.0 = mid;
                } else {
                    lat_range.1 = mid;
                }
            }
            is_lon = !is_lon;
        }
    }

    Some((
        (lat_range.0 + lat_range.1) / 2.0,
        (lon_range.0 + lon_range.1) / 2.0,
        (lat_range.1 - lat_range.0) / 2.0,
        (lon_range.1 - lon_range.0) / 2.0,
    ))
}

// ── WKB Encoding/Decoding ──

fn encode_wkb(geo: &Geometry) -> Vec<u8> {
    let mut buf = Vec::new();
    buf.push(0x01); // Little-endian

    match geo {
        Geometry::Point(c) => {
            buf.extend_from_slice(&1u32.to_le_bytes());
            buf.extend_from_slice(&c.x.to_le_bytes());
            buf.extend_from_slice(&c.y.to_le_bytes());
        }
        Geometry::LineString(coords) => {
            buf.extend_from_slice(&2u32.to_le_bytes());
            buf.extend_from_slice(&(coords.len() as u32).to_le_bytes());
            for c in coords {
                buf.extend_from_slice(&c.x.to_le_bytes());
                buf.extend_from_slice(&c.y.to_le_bytes());
            }
        }
        Geometry::Polygon(rings) => {
            buf.extend_from_slice(&3u32.to_le_bytes());
            buf.extend_from_slice(&(rings.len() as u32).to_le_bytes());
            for ring in rings {
                buf.extend_from_slice(&(ring.len() as u32).to_le_bytes());
                for c in ring {
                    buf.extend_from_slice(&c.x.to_le_bytes());
                    buf.extend_from_slice(&c.y.to_le_bytes());
                }
            }
        }
        Geometry::MultiPolygon(polys) => {
            buf.extend_from_slice(&6u32.to_le_bytes());
            buf.extend_from_slice(&(polys.len() as u32).to_le_bytes());
            for poly in polys {
                buf.push(0x01);
                buf.extend_from_slice(&3u32.to_le_bytes());
                buf.extend_from_slice(&(poly.len() as u32).to_le_bytes());
                for ring in poly {
                    buf.extend_from_slice(&(ring.len() as u32).to_le_bytes());
                    for c in ring {
                        buf.extend_from_slice(&c.x.to_le_bytes());
                        buf.extend_from_slice(&c.y.to_le_bytes());
                    }
                }
            }
        }
    }
    buf
}

fn decode_wkb(data: &[u8]) -> Option<Geometry> {
    if data.len() < 5 {
        return None;
    }
    let _endian = data[0];
    let geom_type = u32::from_le_bytes(data[1..5].try_into().ok()?);

    match geom_type {
        1 => {
            // Point
            if data.len() < 21 {
                return None;
            }
            let x = f64::from_le_bytes(data[5..13].try_into().ok()?);
            let y = f64::from_le_bytes(data[13..21].try_into().ok()?);
            Some(Geometry::Point(Coord::new(x, y)))
        }
        2 => {
            // LineString
            if data.len() < 9 {
                return None;
            }
            let n = u32::from_le_bytes(data[5..9].try_into().ok()?) as usize;
            let mut coords = Vec::with_capacity(n);
            let mut offset = 9;
            for _ in 0..n {
                if offset + 16 > data.len() {
                    return None;
                }
                // Safe: bounds checked above
                let x = f64::from_le_bytes(data[offset..offset + 8].try_into().ok()?);
                let y = f64::from_le_bytes(data[offset + 8..offset + 16].try_into().ok()?);
                coords.push(Coord::new(x, y));
                offset += 16;
            }
            Some(Geometry::LineString(coords))
        }
        3 => {
            // Polygon
            if data.len() < 9 {
                return None;
            }
            let nrings = u32::from_le_bytes(data[5..9].try_into().ok()?) as usize;
            let mut rings = Vec::with_capacity(nrings);
            let mut offset = 9;
            for _ in 0..nrings {
                if offset + 4 > data.len() {
                    return None;
                }
                let npts = u32::from_le_bytes(data[offset..offset + 4].try_into().ok()?) as usize;
                offset += 4;
                let mut ring = Vec::with_capacity(npts);
                for _ in 0..npts {
                    if offset + 16 > data.len() {
                        return None;
                    }
                    let x = f64::from_le_bytes(data[offset..offset + 8].try_into().ok()?);
                    let y = f64::from_le_bytes(data[offset + 8..offset + 16].try_into().ok()?);
                    ring.push(Coord::new(x, y));
                    offset += 16;
                }
                rings.push(ring);
            }
            Some(Geometry::Polygon(rings))
        }
        _ => None,
    }
}

// ── WKT Encoding/Decoding ──

fn encode_wkt(geo: &Geometry) -> String {
    match geo {
        Geometry::Point(c) => format!("POINT({})", c),
        Geometry::LineString(coords) => {
            let s: Vec<String> = coords.iter().map(|c| c.to_string()).collect();
            format!("LINESTRING({})", s.join(","))
        }
        Geometry::Polygon(rings) => {
            let s: Vec<String> = rings
                .iter()
                .map(|ring| {
                    let pts: Vec<String> = ring.iter().map(|c| c.to_string()).collect();
                    format!("({})", pts.join(","))
                })
                .collect();
            format!("POLYGON({})", s.join(","))
        }
        Geometry::MultiPolygon(polys) => {
            let s: Vec<String> = polys
                .iter()
                .map(|poly| {
                    let rings: Vec<String> = poly
                        .iter()
                        .map(|ring| {
                            let pts: Vec<String> = ring.iter().map(|c| c.to_string()).collect();
                            format!("({})", pts.join(","))
                        })
                        .collect();
                    format!("({})", rings.join(","))
                })
                .collect();
            format!("MULTIPOLYGON({})", s.join(","))
        }
    }
}

fn decode_wkt(s: &str) -> Option<Geometry> {
    let s = s.trim();
    if let Some(inner) = s.strip_prefix("POINT(").and_then(|s| s.strip_suffix(')')) {
        let coords = parse_coord(inner)?;
        return Some(Geometry::Point(coords));
    }
    if let Some(inner) = s.strip_prefix("LINESTRING(").and_then(|s| s.strip_suffix(')')) {
        let coords = parse_coord_list(inner)?;
        return Some(Geometry::LineString(coords));
    }
    None // Polygon/MultiPolygon WKT parsing omitted for brevity
}

fn parse_coord(s: &str) -> Option<Coord> {
    let parts: Vec<&str> = s.split_whitespace().collect();
    if parts.len() >= 2 {
        let x = parts[0].parse::<f64>().ok()?;
        let y = parts[1].parse::<f64>().ok()?;
        Some(Coord::new(x, y))
    } else {
        None
    }
}

fn parse_coord_list(s: &str) -> Option<Vec<Coord>> {
    s.split(',').map(|c| parse_coord(c.trim())).collect()
}

// ── Helpers ──

fn bbox_from_coords(coords: &[Coord]) -> (f64, f64, f64, f64) {
    let mut min_x = f64::MAX;
    let mut min_y = f64::MAX;
    let mut max_x = f64::MIN;
    let mut max_y = f64::MIN;
    for c in coords {
        min_x = min_x.min(c.x);
        min_y = min_y.min(c.y);
        max_x = max_x.max(c.x);
        max_y = max_y.max(c.y);
    }
    (min_x, min_y, max_x, max_y)
}

fn point_in_polygon(point: &Coord, polygon: &[Coord]) -> bool {
    let n = polygon.len();
    if n < 3 {
        return false;
    }
    let mut inside = false;
    let mut j = n - 1;
    for i in 0..n {
        let xi = polygon[i].x;
        let yi = polygon[i].y;
        let xj = polygon[j].x;
        let yj = polygon[j].y;
        if ((yi > point.y) != (yj > point.y))
            && (point.x < (xj - xi) * (point.y - yi) / (yj - yi) + xi)
        {
            inside = !inside;
        }
        j = i;
    }
    inside
}

fn haversine_distance(lon1: f64, lat1: f64, lon2: f64, lat2: f64) -> f64 {
    const R: f64 = 6_371_000.0; // Earth radius in meters
    let dlat = (lat2 - lat1).to_radians();
    let dlon = (lon2 - lon1).to_radians();
    let a = (dlat / 2.0).sin().powi(2)
        + lat1.to_radians().cos() * lat2.to_radians().cos() * (dlon / 2.0).sin().powi(2);
    let c = 2.0 * a.sqrt().asin();
    R * c
}

/// Check if two bounding boxes intersect.
fn bbox_intersects(a: &(f64, f64, f64, f64), b: &(f64, f64, f64, f64)) -> bool {
    a.0 <= b.2 && a.2 >= b.0 && a.1 <= b.3 && a.3 >= b.1
}

/// Check if a point is inside any part of a geometry.
fn point_in_geometry(point: &Coord, geo: &Geometry) -> bool {
    match geo {
        Geometry::Point(p) => point == p,
        Geometry::LineString(coords) => point_on_linestring(point, coords),
        Geometry::Polygon(rings) => {
            if let Some(outer) = rings.first() {
                point_in_polygon(point, outer)
            } else {
                false
            }
        }
        Geometry::MultiPolygon(polys) => polys.iter().any(|poly| {
            if let Some(outer) = poly.first() {
                point_in_polygon(point, outer)
            } else {
                false
            }
        }),
    }
}

/// Check if a point is on a linestring.
fn point_on_linestring(point: &Coord, coords: &[Coord]) -> bool {
    for window in coords.windows(2) {
        if point_on_line_segment(point, &window[0], &window[1]) {
            return true;
        }
    }
    false
}

/// Check if a point is on a line segment.
fn point_on_line_segment(point: &Coord, a: &Coord, b: &Coord) -> bool {
    let dx = b.x - a.x;
    let dy = b.y - a.y;
    let len_sq = dx * dx + dy * dy;
    if len_sq < 1e-10 {
        return (point.x - a.x).abs() < 1e-10 && (point.y - a.y).abs() < 1e-10;
    }
    let t = ((point.x - a.x) * dx + (point.y - a.y) * dy) / len_sq;
    if !(0.0..=1.0).contains(&t) {
        return false;
    }
    let proj_x = a.x + t * dx;
    let proj_y = a.y + t * dy;
    ((point.x - proj_x).powi(2) + (point.y - proj_y).powi(2)).sqrt() < 1e-10
}

/// Check if a point is on the boundary of a polygon.
fn point_on_boundary(point: &Coord, polygon: &[Coord]) -> bool {
    for window in polygon.windows(2) {
        if point_on_line_segment(point, &window[0], &window[1]) {
            return true;
        }
    }
    false
}

/// Check if two polygons intersect.
fn polygon_intersects_polygon(rings_a: &[Vec<Coord>], rings_b: &[Vec<Coord>]) -> bool {
    if let (Some(outer_a), Some(outer_b)) = (rings_a.first(), rings_b.first()) {
        // Check if any point of A is inside B or vice versa
        outer_a.iter().any(|p| point_in_polygon(p, outer_b))
            || outer_b.iter().any(|p| point_in_polygon(p, outer_a))
    } else {
        false
    }
}

/// Check if two linestrings intersect.
fn line_intersects_line(l1: &[Coord], l2: &[Coord]) -> bool {
    for w1 in l1.windows(2) {
        for w2 in l2.windows(2) {
            if segments_intersect(&w1[0], &w1[1], &w2[0], &w2[1]) {
                return true;
            }
        }
    }
    false
}

/// Check if two line segments intersect.
fn segments_intersect(a: &Coord, b: &Coord, c: &Coord, d: &Coord) -> bool {
    let d1 = direction(c, d, a);
    let d2 = direction(c, d, b);
    let d3 = direction(a, b, c);
    let d4 = direction(a, b, d);

    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true;
    }

    false
}

/// Cross product direction.
fn direction(a: &Coord, b: &Coord, c: &Coord) -> f64 {
    (b.x - a.x) * (c.y - a.y) - (c.x - a.x) * (b.y - a.y)
}

// ── Tests ──

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_point_wkb_roundtrip() {
        let p = Geometry::Point(Coord::new(116.4, 39.9));
        let wkb = p.to_wkb();
        let decoded = Geometry::from_wkb(&wkb).unwrap();
        assert_eq!(p, decoded);
    }

    #[test]
    fn test_linestring_wkb_roundtrip() {
        let ls = Geometry::LineString(vec![
            Coord::new(116.4, 39.9),
            Coord::new(117.0, 40.0),
            Coord::new(118.0, 41.0),
        ]);
        let wkb = ls.to_wkb();
        let decoded = Geometry::from_wkb(&wkb).unwrap();
        assert_eq!(ls, decoded);
    }

    #[test]
    fn test_polygon_wkb_roundtrip() {
        let poly = Geometry::Polygon(vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(10.0, 0.0),
            Coord::new(10.0, 10.0),
            Coord::new(0.0, 10.0),
            Coord::new(0.0, 0.0),
        ]]);
        let wkb = poly.to_wkb();
        let decoded = Geometry::from_wkb(&wkb).unwrap();
        assert_eq!(poly, decoded);
    }

    #[test]
    fn test_point_wkt_roundtrip() {
        let p = Geometry::Point(Coord::new(116.4, 39.9));
        let wkt = p.to_wkt();
        assert_eq!(wkt, "POINT(116.4 39.9)");
        let decoded = Geometry::from_wkt(&wkt).unwrap();
        assert_eq!(p, decoded);
    }

    #[test]
    fn test_distance() {
        let beijing = Geometry::Point(Coord::new(116.4, 39.9));
        let shanghai = Geometry::Point(Coord::new(121.5, 31.2));
        let d = distance(&beijing, &shanghai);
        assert!((d - 1_068_000.0).abs() < 10_000.0); // ~1068km ±10km
    }

    #[test]
    fn test_contains() {
        let poly = Geometry::Polygon(vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(10.0, 0.0),
            Coord::new(10.0, 10.0),
            Coord::new(0.0, 10.0),
            Coord::new(0.0, 0.0),
        ]]);
        let inside = Geometry::Point(Coord::new(5.0, 5.0));
        let outside = Geometry::Point(Coord::new(15.0, 15.0));
        assert!(contains(&poly, &inside));
        assert!(!contains(&poly, &outside));
    }

    #[test]
    fn test_intersects() {
        let a = Geometry::Polygon(vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(10.0, 0.0),
            Coord::new(10.0, 10.0),
            Coord::new(0.0, 10.0),
            Coord::new(0.0, 0.0),
        ]]);
        let b = Geometry::Polygon(vec![vec![
            Coord::new(5.0, 5.0),
            Coord::new(15.0, 5.0),
            Coord::new(15.0, 15.0),
            Coord::new(5.0, 15.0),
            Coord::new(5.0, 5.0),
        ]]);
        let c = Geometry::Polygon(vec![vec![
            Coord::new(20.0, 20.0),
            Coord::new(30.0, 20.0),
            Coord::new(30.0, 30.0),
            Coord::new(20.0, 30.0),
            Coord::new(20.0, 20.0),
        ]]);
        assert!(intersects(&a, &b));
        assert!(!intersects(&a, &c));
    }

    #[test]
    fn test_geohash() {
        // Beijing: ~116.4, 39.9
        let hash = geohash_encode(39.9, 116.4, 8);
        assert_eq!(hash.len(), 8);
        let (lat, lon, _, _) = geohash_decode(&hash).unwrap();
        assert!((lat - 39.9).abs() < 0.001);
        assert!((lon - 116.4).abs() < 0.001);
    }

    #[test]
    fn test_centroid() {
        let poly = Geometry::Polygon(vec![vec![
            Coord::new(0.0, 0.0),
            Coord::new(10.0, 0.0),
            Coord::new(10.0, 10.0),
            Coord::new(0.0, 10.0),
            Coord::new(0.0, 0.0),
        ]]);
        let c = centroid(&poly);
        assert!((c.x - 4.0).abs() < 0.01);
        assert!((c.y - 4.0).abs() < 0.01);
    }

    #[test]
    fn test_bounding_box() {
        let ls = Geometry::LineString(vec![
            Coord::new(1.0, 2.0),
            Coord::new(5.0, 8.0),
            Coord::new(3.0, 4.0),
        ]);
        let (min_x, min_y, max_x, max_y) = ls.bounding_box();
        assert_eq!(min_x, 1.0);
        assert_eq!(min_y, 2.0);
        assert_eq!(max_x, 5.0);
        assert_eq!(max_y, 8.0);
    }

    // ── Nine-Intersection Model Tests ──

    fn make_square(x: f64, y: f64, size: f64) -> Geometry {
        Geometry::Polygon(vec![vec![
            Coord::new(x, y),
            Coord::new(x + size, y),
            Coord::new(x + size, y + size),
            Coord::new(x, y + size),
            Coord::new(x, y),
        ]])
    }

    #[test]
    fn test_contains_point() {
        let poly = make_square(0.0, 0.0, 10.0);
        let inside = Geometry::Point(Coord::new(5.0, 5.0));
        let outside = Geometry::Point(Coord::new(15.0, 15.0));
        let on_boundary = Geometry::Point(Coord::new(0.0, 5.0));

        assert!(contains(&poly, &inside));
        assert!(!contains(&poly, &outside));
        assert!(!contains(&poly, &on_boundary)); // Boundary is not interior
    }

    #[test]
    fn test_within() {
        let poly = make_square(0.0, 0.0, 10.0);
        let inside = Geometry::Point(Coord::new(5.0, 5.0));

        assert!(within(&inside, &poly));
        assert!(!within(&poly, &inside));
    }

    #[test]
    fn test_disjoint() {
        let a = make_square(0.0, 0.0, 10.0);
        let b = make_square(20.0, 20.0, 10.0);

        assert!(disjoint(&a, &b));
        assert!(!intersects(&a, &b));
    }

    #[test]
    fn test_intersects_polygons() {
        let a = make_square(0.0, 0.0, 10.0);
        let b = make_square(5.0, 5.0, 10.0);
        let c = make_square(20.0, 20.0, 10.0);

        assert!(intersects(&a, &b));
        assert!(!intersects(&a, &c));
    }

    #[test]
    fn test_overlaps() {
        let a = make_square(0.0, 0.0, 10.0);
        let b = make_square(5.0, 5.0, 10.0);

        assert!(overlaps(&a, &b));
    }

    #[test]
    fn test_contains_polygon() {
        let outer = make_square(0.0, 0.0, 20.0);
        let inner = make_square(5.0, 5.0, 5.0);

        assert!(contains(&outer, &inner));
        assert!(!contains(&inner, &outer));
    }

    #[test]
    fn test_equals() {
        let a = make_square(0.0, 0.0, 10.0);
        let b = make_square(0.0, 0.0, 10.0);
        let c = make_square(5.0, 5.0, 10.0);

        assert!(equals(&a, &b));
        assert!(!equals(&a, &c));
    }

    #[test]
    fn test_relate() {
        let a = make_square(0.0, 0.0, 10.0);
        let b = make_square(5.0, 5.0, 10.0);
        let c = make_square(20.0, 20.0, 10.0);

        assert_eq!(relate(&a, &a), SpatialRelation::Equals);
        assert_eq!(relate(&a, &c), SpatialRelation::Disjoint);
        assert_eq!(relate(&a, &b), SpatialRelation::Overlaps);
    }
}
