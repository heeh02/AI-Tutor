//! Memory-only background translation estimate. No device, filesystem or image output.
//!
//! Positive dx/dy means image content moved right/down in `after` relative to `before`:
//! `after(x + dx, y + dy)` corresponds to `before(x, y)`.
use image::{GrayImage, ImageFormat, ImageReader, Limits, imageops::FilterType};
use serde::Serialize;
use std::io::Cursor;

const WIDTH: u32 = 160;
const HEIGHT: u32 = 90;
const RADIUS: i32 = 12;
const EDGE: i32 = 14;
const CENTER: [i32; 4] = [44, 10, 116, 80];
const MAX_JPEG_BYTES: usize = 5 * 1024 * 1024;
const MIN_PIXELS: usize = 1400;
const MIN_SIDE_PIXELS: usize = 500;
const MIN_TEXTURE: f64 = 10.0;
const MIN_SIDE_TEXTURE: f64 = 6.0;
const MIN_NCC: f64 = 0.90;
const MIN_SIDE_NCC: f64 = 0.80;
const MIN_PEAK_MARGIN: f64 = 0.035;
const MAX_MEAN_CHANGE: f64 = 18.0;
const MAX_MEAN_ABSOLUTE_ERROR: f64 = 16.0;

#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RejectionReason {
    InsufficientBackgroundTexture,
    InsufficientOverlap,
    ExposureChanged,
    SceneChanged,
    BackgroundDisagreement,
    SearchBoundary,
    AmbiguousPeak,
    NoIndependentBackgroundPatches,
    NonStationaryBackground,
    PatchMotionDisagreement,
    FaceOverlapsBackground,
    InsufficientTwoDimensionalTexture,
}

const MIN_GRADIENT_EIGENVALUE: f64 = 4.0;
const MAX_STATIC_SHIFT: f64 = 0.25;
const MAX_PATCH_DISAGREEMENT: f64 = 0.75;

#[derive(Debug, Clone, Serialize)]
pub struct BackgroundPlan {
    pub source_width: u32,
    pub source_height: u32,
    pub regions: Vec<[i32; 4]>,
    /// History of exclusion geometry only, never face identity.
    pub preflight_face_masks: Vec<[i32; 4]>,
}
#[derive(Debug, Clone, Serialize)]
pub struct PatchEvidence {
    pub region: [i32; 4],
    pub pixels: usize,
    pub evaluated_candidates: usize,
    pub before_stddev: f64,
    pub after_stddev: f64,
    pub before_gradient_min_eigenvalue: f64,
    pub after_gradient_min_eigenvalue: f64,
    pub similarity: Option<f64>,
    pub second_peak_similarity: Option<f64>,
    pub peak_margin: Option<f64>,
    pub mean_absolute_error: Option<f64>,
    pub mean_brightness_change: Option<f64>,
    pub contrast_ratio: Option<f64>,
    pub dx: Option<f64>,
    pub dy: Option<f64>,
    pub decidable: bool,
    pub rejection_reason: Option<RejectionReason>,
}
impl PatchEvidence {
    fn empty(region: [i32; 4]) -> Self {
        Self {
            region,
            pixels: 0,
            evaluated_candidates: 0,
            before_stddev: 0.0,
            after_stddev: 0.0,
            before_gradient_min_eigenvalue: 0.0,
            after_gradient_min_eigenvalue: 0.0,
            similarity: None,
            second_peak_similarity: None,
            peak_margin: None,
            mean_absolute_error: None,
            mean_brightness_change: None,
            contrast_ratio: None,
            dx: None,
            dy: None,
            decidable: false,
            rejection_reason: None,
        }
    }
    fn reject(mut self, reason: RejectionReason) -> Self {
        self.rejection_reason = Some(reason);
        self
    }
}
#[derive(Debug, Clone, Serialize)]
pub struct BackgroundMotion {
    pub motion: MotionEstimate,
    pub patches: Vec<PatchEvidence>,
    pub candidate_count: usize,
    pub mask_strategy: &'static str,
    pub minimum_independent_patches: usize,
    pub static_shift_limit_pixels: f64,
    pub patch_disagreement_limit_pixels: f64,
    pub gradient_min_eigenvalue_threshold: f64,
}
#[derive(Debug, Clone, Serialize)]
pub struct BackgroundPreparation {
    pub plan: Option<BackgroundPlan>,
    pub quality: BackgroundMotion,
}

fn face_masks(boxes: &[[f64; 4]]) -> Result<Vec<[i32; 4]>, String> {
    if boxes.len() > 32
        || boxes
            .iter()
            .flatten()
            .any(|v| !v.is_finite() || !(0.0..=1.0).contains(v))
    {
        return Err("Invalid normalized face exclusion geometry".into());
    }
    let mut masks = Vec::with_capacity(boxes.len());
    for b in boxes {
        if b[0] >= b[2] || b[1] >= b[3] {
            return Err("Face exclusion rectangle is inverted or empty".into());
        }
        let px = (b[2] - b[0]) * 0.15;
        let py = (b[3] - b[1]) * 0.15;
        masks.push([
            (((b[0] - px).max(0.0) * f64::from(WIDTH)).floor() as i32 - 2).max(0),
            (((b[1] - py).max(0.0) * f64::from(HEIGHT)).floor() as i32 - 2).max(0),
            (((b[2] + px).min(1.0) * f64::from(WIDTH)).ceil() as i32 + 2).min(WIDTH as i32),
            (((b[3] + py).min(1.0) * f64::from(HEIGHT)).ceil() as i32 + 2).min(HEIGHT as i32),
        ]);
    }
    Ok(masks)
}
fn overlap(a: [i32; 4], b: [i32; 4]) -> bool {
    a[0] < b[2] && a[2] > b[0] && a[1] < b[3] && a[3] > b[1]
}
fn point_masked(x: i32, y: i32, masks: &[[i32; 4]]) -> bool {
    masks
        .iter()
        .any(|m| x >= m[0] && x < m[2] && y >= m[1] && y < m[3])
}
fn gradient_min(gray: &[u8], r: [i32; 4]) -> f64 {
    let (mut n, mut xx, mut yy, mut xy) = (0usize, 0.0f64, 0.0f64, 0.0f64);
    for y in r[1].max(0)..(r[3] - 1).min(HEIGHT as i32 - 1) {
        for x in r[0].max(0)..(r[2] - 1).min(WIDTH as i32 - 1) {
            let i = (y * WIDTH as i32 + x) as usize;
            let gx = f64::from(gray[i + 1]) - f64::from(gray[i]);
            let gy = f64::from(gray[i + WIDTH as usize]) - f64::from(gray[i]);
            xx += gx * gx;
            yy += gy * gy;
            xy += gx * gy;
            n += 1;
        }
    }
    let n = n.max(1) as f64;
    xx /= n;
    yy /= n;
    xy /= n;
    ((xx + yy - ((xx - yy) * (xx - yy) + 4.0 * xy * xy).sqrt()) / 2.0).max(0.0)
}
fn patch_compare(
    before: &[u8],
    after: &[u8],
    r: [i32; 4],
    masks: &[[i32; 4]],
    dx: i32,
    dy: i32,
) -> Option<Candidate> {
    let mut stats = Stats::default();
    for y in r[1]..r[3] {
        for x in r[0]..r[2] {
            let (ax, ay) = (x + dx, y + dy);
            if ax < EDGE
                || ax >= WIDTH as i32 - EDGE
                || ay < EDGE
                || ay >= HEIGHT as i32 - EDGE
                || point_masked(x, y, masks)
                || point_masked(ax, ay, masks)
            {
                continue;
            }
            stats.add(
                before[(y * WIDTH as i32 + x) as usize],
                after[(ay * WIDTH as i32 + ax) as usize],
            );
        }
    }
    if stats.n < MIN_SIDE_PIXELS {
        return None;
    }
    Some(Candidate {
        dx,
        dy,
        stats,
        left: Stats::default(),
        right: Stats::default(),
        score: stats.ncc()?,
    })
}
fn analyze_patch(before: &[u8], after: &[u8], r: [i32; 4], masks: &[[i32; 4]]) -> PatchEvidence {
    let mut result = PatchEvidence::empty(r);
    if masks.iter().any(|m| overlap(r, *m)) {
        return result.reject(RejectionReason::FaceOverlapsBackground);
    }
    result.before_gradient_min_eigenvalue = gradient_min(before, r);
    result.after_gradient_min_eigenvalue = gradient_min(after, r);
    let mut zero = Stats::default();
    for y in r[1]..r[3] {
        for x in r[0]..r[2] {
            let i = (y * WIDTH as i32 + x) as usize;
            zero.add(before[i], after[i]);
        }
    }
    result.pixels = zero.n;
    (result.before_stddev, result.after_stddev) = zero.stddevs();
    if result.before_stddev < MIN_SIDE_TEXTURE || result.after_stddev < MIN_SIDE_TEXTURE {
        return result.reject(RejectionReason::InsufficientBackgroundTexture);
    }
    if result.before_gradient_min_eigenvalue < MIN_GRADIENT_EIGENVALUE
        || result.after_gradient_min_eigenvalue < MIN_GRADIENT_EIGENVALUE
    {
        return result.reject(RejectionReason::InsufficientTwoDimensionalTexture);
    }
    let mut candidates = Vec::with_capacity(625);
    for dy in -RADIUS..=RADIUS {
        for dx in -RADIUS..=RADIUS {
            if let Some(candidate) = patch_compare(before, after, r, masks, dx, dy) {
                candidates.push(candidate);
            }
        }
    }
    result.evaluated_candidates = candidates.len();
    let Some(best) = candidates
        .iter()
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .copied()
    else {
        return result.reject(RejectionReason::InsufficientOverlap);
    };
    let second = candidates
        .iter()
        .filter(|c| (c.dx - best.dx).abs().max((c.dy - best.dy).abs()) >= 3)
        .map(|c| c.score)
        .max_by(f64::total_cmp);
    result.similarity = Some(best.score);
    result.second_peak_similarity = second;
    result.peak_margin = second.map(|s| best.score - s);
    result.pixels = best.stats.n;
    let (a, b) = best.stats.means();
    let (sa, sb) = best.stats.stddevs();
    let contrast = sb / sa.max(1e-9);
    let error = best.stats.absolute_error / best.stats.n as f64;
    result.before_stddev = sa;
    result.after_stddev = sb;
    result.mean_absolute_error = Some(error);
    result.mean_brightness_change = Some(b - a);
    result.contrast_ratio = Some(contrast);
    if sa < MIN_SIDE_TEXTURE || sb < MIN_SIDE_TEXTURE {
        return result.reject(RejectionReason::InsufficientBackgroundTexture);
    }
    if (b - a).abs() > MAX_MEAN_CHANGE || !(0.8..=1.25).contains(&contrast) {
        return result.reject(RejectionReason::ExposureChanged);
    }
    if best.score < MIN_NCC || error > MAX_MEAN_ABSOLUTE_ERROR {
        return result.reject(RejectionReason::SceneChanged);
    }
    if best.dx.abs() == RADIUS || best.dy.abs() == RADIUS {
        return result.reject(RejectionReason::SearchBoundary);
    }
    if result
        .peak_margin
        .is_none_or(|margin| margin < MIN_PEAK_MARGIN)
    {
        return result.reject(RejectionReason::AmbiguousPeak);
    }
    result.dx = Some(best.dx as f64 + subpixel(&candidates, best, true));
    result.dy = Some(best.dy as f64 + subpixel(&candidates, best, false));
    result.decidable = true;
    result
}
fn aggregate_patches(
    width: u32,
    height: u32,
    patches: Vec<PatchEvidence>,
    candidate_count: usize,
    static_check: bool,
) -> BackgroundMotion {
    let pixels = patches.iter().map(|p| p.pixels).sum::<usize>();
    let rms = |before: bool| {
        (patches
            .iter()
            .map(|p| {
                let s = if before {
                    p.before_stddev
                } else {
                    p.after_stddev
                };
                s * s * p.pixels as f64
            })
            .sum::<f64>()
            / pixels.max(1) as f64)
            .sqrt()
    };
    let mut motion = MotionEstimate::initial(width, height, rms(true), rms(false));
    motion.excluded_center_xyxy = [0; 4];
    motion.compared_pixels = pixels;
    motion.evaluated_candidates = patches.iter().map(|patch| patch.evaluated_candidates).sum();
    motion.similarity = patches
        .iter()
        .filter_map(|p| p.similarity)
        .min_by(f64::total_cmp);
    motion.peak_margin = patches
        .iter()
        .filter_map(|p| p.peak_margin)
        .min_by(f64::total_cmp);
    motion.mean_absolute_error = patches
        .iter()
        .filter_map(|p| p.mean_absolute_error)
        .max_by(f64::total_cmp);
    motion.mean_brightness_change = patches
        .iter()
        .filter_map(|p| p.mean_brightness_change)
        .max_by(|a, b| a.abs().total_cmp(&b.abs()));
    let reason = if patches.len() < 2 || pixels < MIN_PIXELS {
        Some(RejectionReason::NoIndependentBackgroundPatches)
    } else if let Some(patch) = patches.iter().find(|p| !p.decidable) {
        patch.rejection_reason.clone()
    } else if motion.before_texture_stddev < MIN_TEXTURE
        || motion.after_texture_stddev < MIN_TEXTURE
    {
        Some(RejectionReason::InsufficientBackgroundTexture)
    } else {
        let mut reason = None;
        for a in &patches {
            for b in &patches {
                let (ax, ay, bx, by) = (a.dx.unwrap(), a.dy.unwrap(), b.dx.unwrap(), b.dy.unwrap());
                if (ax - bx).abs() > MAX_PATCH_DISAGREEMENT
                    || (ay - by).abs() > MAX_PATCH_DISAGREEMENT
                    || (ax.abs() >= 0.5 && bx.abs() >= 0.5 && ax * bx <= 0.0)
                    || (ay.abs() >= 0.5 && by.abs() >= 0.5 && ay * by <= 0.0)
                {
                    reason = Some(RejectionReason::PatchMotionDisagreement);
                }
            }
        }
        if static_check
            && patches.iter().any(|p| {
                p.dx.unwrap().abs() > MAX_STATIC_SHIFT || p.dy.unwrap().abs() > MAX_STATIC_SHIFT
            })
        {
            reason = Some(RejectionReason::NonStationaryBackground);
        }
        reason
    };
    if let Some(reason) = reason {
        motion = motion.reject(reason);
    } else {
        motion.decidable = true;
        let dx = patches.iter().map(|p| p.dx.unwrap()).sum::<f64>() / patches.len() as f64;
        let dy = patches.iter().map(|p| p.dy.unwrap()).sum::<f64>() / patches.len() as f64;
        motion.dx = Some(dx);
        motion.dy = Some(dy);
        motion.dx_source_pixels = Some(dx * width as f64 / WIDTH as f64);
        motion.dy_source_pixels = Some(dy * height as f64 / HEIGHT as f64);
    }
    BackgroundMotion {
        motion,
        patches,
        candidate_count,
        mask_strategy: "union of current before/after and preflight face boxes; 15% expansion plus 2 analysis pixels; fixed nonoverlapping per-patch zero-mean NCC",
        minimum_independent_patches: 2,
        static_shift_limit_pixels: MAX_STATIC_SHIFT,
        patch_disagreement_limit_pixels: MAX_PATCH_DISAGREEMENT,
        gradient_min_eigenvalue_threshold: MIN_GRADIENT_EIGENVALUE,
    }
}

/// Select fixed ROIs using TWO distinct no-motion frames. No physical command
/// is warranted by this plan; it only records locally consistent image evidence.
fn best_background_pair(
    width: u32,
    height: u32,
    candidates: &[PatchEvidence],
    candidate_count: usize,
) -> Option<(usize, usize)> {
    let rank = |p: &PatchEvidence| {
        p.peak_margin.unwrap_or(0.0)
            * p.before_stddev.min(p.after_stddev)
            * p.before_gradient_min_eigenvalue
                .min(p.after_gradient_min_eigenvalue)
                .sqrt()
    };
    let mut best_pair = None;
    let mut best_rank = -1.0f64;
    for i in 0..candidates.len() {
        for j in i + 1..candidates.len() {
            if overlap(candidates[i].region, candidates[j].region) {
                continue;
            }
            let pair = aggregate_patches(
                width,
                height,
                vec![candidates[i].clone(), candidates[j].clone()],
                candidate_count,
                true,
            );
            let score = rank(&candidates[i]).min(rank(&candidates[j]));
            if pair.motion.decidable && score > best_rank {
                best_rank = score;
                best_pair = Some((i, j));
            }
        }
    }
    best_pair
}

fn grid_origins(maximum: i32) -> Vec<i32> {
    let mut positions = (EDGE..=maximum).step_by(12).collect::<Vec<_>>();
    if positions.last().copied() != Some(maximum) {
        positions.push(maximum);
    }
    positions
}

fn stationary_candidates(
    before: &[u8],
    after: &[u8],
    regions: &[[i32; 4]],
    masks: &[[i32; 4]],
) -> Vec<PatchEvidence> {
    regions
        .iter()
        .map(|r| analyze_patch(before, after, *r, masks))
        .filter(|patch| {
            patch.decidable
                && patch.dx.unwrap().abs() <= MAX_STATIC_SHIFT
                && patch.dy.unwrap().abs() <= MAX_STATIC_SHIFT
        })
        .collect()
}

fn distributed_patch_texture(gray: &[u8], region: [i32; 4]) -> bool {
    let mid_x = (region[0] + region[2]) / 2;
    let mid_y = (region[1] + region[3]) / 2;
    for [left, right] in [[region[0], mid_x], [mid_x, region[2]]] {
        for [top, bottom] in [[region[1], mid_y], [mid_y, region[3]]] {
            let quadrant = [left, top, right, bottom];
            let mut stats = Stats::default();
            for y in top..bottom {
                for x in left..right {
                    let value = gray[(y * WIDTH as i32 + x) as usize];
                    stats.add(value, value);
                }
            }
            if stats.stddevs().0 < MIN_SIDE_TEXTURE
                || gradient_min(gray, quadrant) < MIN_GRADIENT_EIGENVALUE
            {
                return false;
            }
        }
    }
    true
}

pub fn prepare_background(
    before_jpeg: &[u8],
    after_jpeg: &[u8],
    before_faces: &[[f64; 4]],
    after_faces: &[[f64; 4]],
) -> Result<BackgroundPreparation, String> {
    let (before, width, height) = decode(before_jpeg)?;
    let (after, aw, ah) = decode(after_jpeg)?;
    if (width, height) != (aw, ah) {
        return Err("Preflight capture geometry changed".into());
    }
    let mut masks = face_masks(before_faces)?;
    masks.extend(face_masks(after_faces)?);
    let mut regions = Vec::new();
    for y in [14, 26, 38, 50] {
        for x in [14, 26, 38, 50, 62, 74, 86, 98, 106] {
            regions.push([x, y, x + 40, y + 24]);
        }
    }
    let mut candidate_count = regions.len();
    let mut candidates = stationary_candidates(before.as_raw(), after.as_raw(), &regions, &masks);
    let mut best_pair = best_background_pair(width, height, &candidates, candidate_count);
    if best_pair.is_none() {
        // Preserve the original 40x24 plan whenever it works. Smaller spatial
        // windows only expand the search; every photometric/geometric gate and
        // the >=500 pixels per patch / >=1400 total pixels still apply.
        let mut fallback = Vec::new();
        for size in [28, 24] {
            for y in grid_origins(HEIGHT as i32 - EDGE - size) {
                for x in grid_origins(WIDTH as i32 - EDGE - size) {
                    fallback.push([x, y, x + size, y + size]);
                }
            }
        }
        candidate_count += fallback.len();
        candidates.extend(stationary_candidates(
            before.as_raw(),
            after.as_raw(),
            &fallback,
            &masks,
        ));
        // Expanded placement must not split one small textured island into
        // mostly flat containers just to satisfy the existing pixel budget.
        // Reuse the original texture thresholds in all four spatial quadrants,
        // including coarse candidates considered together with a smaller one.
        candidates.retain(|patch| {
            distributed_patch_texture(before.as_raw(), patch.region)
                && distributed_patch_texture(after.as_raw(), patch.region)
        });
        best_pair = best_background_pair(width, height, &candidates, candidate_count);
    }
    let selected = if let Some((i, j)) = best_pair {
        vec![candidates[i].clone(), candidates[j].clone()]
    } else {
        candidates
    };
    let mut quality = aggregate_patches(width, height, selected, candidate_count, true);
    if best_pair.is_none() {
        quality.motion = quality
            .motion
            .reject(RejectionReason::NoIndependentBackgroundPatches);
        quality.motion.decidable = false;
        quality.motion.dx = None;
        quality.motion.dy = None;
        quality.motion.dx_source_pixels = None;
        quality.motion.dy_source_pixels = None;
    }
    let plan = quality.motion.decidable.then(|| BackgroundPlan {
        source_width: width,
        source_height: height,
        regions: quality.patches.iter().map(|p| p.region).collect(),
        preflight_face_masks: masks,
    });
    Ok(BackgroundPreparation { plan, quality })
}

/// Reuse only the selected preflight ROIs. A face entering one, disagreement,
/// exposure changes or ambiguity rejects rather than selecting a new patch.
pub fn estimate_background_motion(
    plan: &BackgroundPlan,
    before_jpeg: &[u8],
    after_jpeg: &[u8],
    before_faces: &[[f64; 4]],
    after_faces: &[[f64; 4]],
) -> Result<BackgroundMotion, String> {
    let (before, width, height) = decode(before_jpeg)?;
    let (after, aw, ah) = decode(after_jpeg)?;
    if (width, height) != (plan.source_width, plan.source_height) || (width, height) != (aw, ah) {
        return Err("Fixed background plan capture geometry changed".into());
    }
    if !(2..=3).contains(&plan.regions.len())
        || plan.regions.iter().any(|r| {
            r[0] < EDGE
                || r[1] < EDGE
                || r[2] > WIDTH as i32 - EDGE
                || r[3] > HEIGHT as i32 - EDGE
                || r[0] >= r[2]
                || r[1] >= r[3]
        })
    {
        return Err("Invalid fixed background plan".into());
    }
    for (i, a) in plan.regions.iter().enumerate() {
        if plan.regions[i + 1..].iter().any(|b| overlap(*a, *b)) {
            return Err("Fixed background ROIs overlap".into());
        }
    }
    let mut masks = plan.preflight_face_masks.clone();
    masks.extend(face_masks(before_faces)?);
    masks.extend(face_masks(after_faces)?);
    let patches = plan
        .regions
        .iter()
        .map(|r| analyze_patch(before.as_raw(), after.as_raw(), *r, &masks))
        .collect();
    Ok(aggregate_patches(
        width,
        height,
        patches,
        plan.regions.len(),
        false,
    ))
}

#[derive(Debug, Clone, Serialize)]
pub struct MotionEstimate {
    pub decidable: bool,
    pub rejection_reason: Option<RejectionReason>,
    /// Analysis-image pixels; None when the estimate is rejected.
    pub dx: Option<f64>,
    pub dy: Option<f64>,
    pub dx_source_pixels: Option<f64>,
    pub dy_source_pixels: Option<f64>,
    /// Zero-mean normalized cross correlation, -1..1; None if not measurable.
    pub similarity: Option<f64>,
    pub second_peak_similarity: Option<f64>,
    pub peak_margin: Option<f64>,
    pub mean_absolute_error: Option<f64>,
    pub mean_brightness_change: Option<f64>,
    pub contrast_ratio: Option<f64>,
    pub left_background_similarity: Option<f64>,
    pub right_background_similarity: Option<f64>,
    pub before_texture_stddev: f64,
    pub after_texture_stddev: f64,
    pub compared_pixels: usize,
    pub evaluated_candidates: usize,
    pub analysis_width: u32,
    pub analysis_height: u32,
    pub source_width: u32,
    pub source_height: u32,
    pub search_radius_pixels: i32,
    pub excluded_edge_pixels: i32,
    pub excluded_center_xyxy: [i32; 4],
    pub motion_definition: &'static str,
}

impl MotionEstimate {
    fn initial(source_width: u32, source_height: u32, before_std: f64, after_std: f64) -> Self {
        Self {
            decidable: false,
            rejection_reason: None,
            dx: None,
            dy: None,
            dx_source_pixels: None,
            dy_source_pixels: None,
            similarity: None,
            second_peak_similarity: None,
            peak_margin: None,
            mean_absolute_error: None,
            mean_brightness_change: None,
            contrast_ratio: None,
            left_background_similarity: None,
            right_background_similarity: None,
            before_texture_stddev: before_std,
            after_texture_stddev: after_std,
            compared_pixels: 0,
            evaluated_candidates: 0,
            analysis_width: WIDTH,
            analysis_height: HEIGHT,
            source_width,
            source_height,
            search_radius_pixels: RADIUS,
            excluded_edge_pixels: EDGE,
            excluded_center_xyxy: CENTER,
            motion_definition: "after(x+dx,y+dy) corresponds to before(x,y); positive dx/right, positive dy/down; analysis pixels",
        }
    }
    fn reject(mut self, reason: RejectionReason) -> Self {
        self.rejection_reason = Some(reason);
        self
    }
}

#[derive(Clone, Copy, Default)]
struct Stats {
    n: usize,
    a: f64,
    b: f64,
    aa: f64,
    bb: f64,
    ab: f64,
    absolute_error: f64,
}
impl Stats {
    fn add(&mut self, a: u8, b: u8) {
        let a = f64::from(a);
        let b = f64::from(b);
        self.n += 1;
        self.a += a;
        self.b += b;
        self.aa += a * a;
        self.bb += b * b;
        self.ab += a * b;
        self.absolute_error += (a - b).abs();
    }
    fn means(&self) -> (f64, f64) {
        let n = self.n.max(1) as f64;
        (self.a / n, self.b / n)
    }
    fn stddevs(&self) -> (f64, f64) {
        let n = self.n.max(1) as f64;
        let (a, b) = self.means();
        (
            (self.aa / n - a * a).max(0.0).sqrt(),
            (self.bb / n - b * b).max(0.0).sqrt(),
        )
    }
    fn ncc(&self) -> Option<f64> {
        let (sa, sb) = self.stddevs();
        if self.n < 2 || sa < 1e-6 || sb < 1e-6 {
            return None;
        }
        let (a, b) = self.means();
        Some(((self.ab / self.n as f64 - a * b) / (sa * sb)).clamp(-1.0, 1.0))
    }
}
#[derive(Clone, Copy)]
struct Candidate {
    dx: i32,
    dy: i32,
    stats: Stats,
    left: Stats,
    right: Stats,
    score: f64,
}

fn allowed(x: i32, y: i32) -> bool {
    x >= EDGE
        && x < WIDTH as i32 - EDGE
        && y >= EDGE
        && y < HEIGHT as i32 - EDGE
        && !(x >= CENTER[0] && x < CENTER[2] && y >= CENTER[1] && y < CENTER[3])
}

fn decode(jpeg: &[u8]) -> Result<(GrayImage, u32, u32), String> {
    if jpeg.len() > MAX_JPEG_BYTES {
        return Err("JPEG exceeds the five-megabyte analysis bound".into());
    }
    let dimensions = ImageReader::with_format(Cursor::new(jpeg), ImageFormat::Jpeg)
        .into_dimensions()
        .map_err(|e| format!("Invalid JPEG header: {e}"))?;
    let (w, h) = dimensions;
    if w < WIDTH || h < HEIGHT || w > 4096 || h > 2160 {
        return Err("JPEG dimensions are outside the160x90..4096x2160 analysis range".into());
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(4096);
    limits.max_image_height = Some(2160);
    limits.max_alloc = Some(64 * 1024 * 1024);
    let mut reader = ImageReader::with_format(Cursor::new(jpeg), ImageFormat::Jpeg);
    reader.limits(limits);
    let decoded = reader
        .decode()
        .map_err(|e| format!("JPEG decode failed: {e}"))?
        .to_luma8();
    Ok((
        image::imageops::resize(&decoded, WIDTH, HEIGHT, FilterType::Triangle),
        w,
        h,
    ))
}

fn compare(before: &[u8], after: &[u8], dx: i32, dy: i32) -> Option<Candidate> {
    let (mut stats, mut left, mut right) = (Stats::default(), Stats::default(), Stats::default());
    for y in EDGE..HEIGHT as i32 - EDGE {
        for x in EDGE..WIDTH as i32 - EDGE {
            let (ax, ay) = (x + dx, y + dy);
            if !allowed(x, y) || !allowed(ax, ay) {
                continue;
            }
            let a = before[(y * WIDTH as i32 + x) as usize];
            let b = after[(ay * WIDTH as i32 + ax) as usize];
            stats.add(a, b);
            if x < CENTER[0] {
                left.add(a, b)
            } else {
                right.add(a, b)
            }
        }
    }
    if stats.n < MIN_PIXELS {
        return None;
    }
    Some(Candidate {
        dx,
        dy,
        stats,
        left,
        right,
        score: stats.ncc()?,
    })
}

fn subpixel(candidates: &[Candidate], best: Candidate, axis_x: bool) -> f64 {
    let coord = |delta: i32| {
        if axis_x {
            (best.dx + delta, best.dy)
        } else {
            (best.dx, best.dy + delta)
        }
    };
    let minus = coord(-1);
    let plus = coord(1);
    let Some(a) = candidates.iter().find(|c| (c.dx, c.dy) == minus) else {
        return 0.0;
    };
    let Some(b) = candidates.iter().find(|c| (c.dx, c.dy) == plus) else {
        return 0.0;
    };
    let curvature = a.score - 2.0 * best.score + b.score;
    if curvature >= -1e-6 {
        return 0.0;
    }
    (0.5 * (a.score - b.score) / curvature).clamp(-0.5, 0.5)
}

/// Estimate a single small translation from static side backgrounds. Rejected results
/// contain quality numbers but no dx/dy, so ambiguous data cannot become motion evidence.
pub fn estimate_motion(before_jpeg: &[u8], after_jpeg: &[u8]) -> Result<MotionEstimate, String> {
    let (before, w, h) = decode(before_jpeg)?;
    let (after, aw, ah) = decode(after_jpeg)?;
    if (w, h) != (aw, ah) {
        return Err(
            "Before/after source dimensions differ; frames are not directly comparable".into(),
        );
    }
    let before = before.as_raw();
    let after = after.as_raw();
    let baseline = compare(before, after, 0, 0);
    let (base_before_std, base_after_std) =
        baseline.map(|c| c.stats.stddevs()).unwrap_or((0.0, 0.0));
    let mut estimate = MotionEstimate::initial(w, h, base_before_std, base_after_std);
    // Flat images have undefined normalized correlation and are never evidence of no movement.
    if base_before_std < MIN_TEXTURE || base_after_std < MIN_TEXTURE {
        return Ok(estimate.reject(RejectionReason::InsufficientBackgroundTexture));
    }
    let mut candidates = Vec::with_capacity(((2 * RADIUS + 1) * (2 * RADIUS + 1)) as usize);
    for dy in -RADIUS..=RADIUS {
        for dx in -RADIUS..=RADIUS {
            if let Some(c) = compare(before, after, dx, dy) {
                candidates.push(c)
            }
        }
    }
    estimate.evaluated_candidates = candidates.len();
    let Some(best) = candidates
        .iter()
        .max_by(|a, b| a.score.total_cmp(&b.score))
        .copied()
    else {
        return Ok(estimate.reject(RejectionReason::InsufficientOverlap));
    };
    let second = candidates
        .iter()
        .filter(|c| (c.dx - best.dx).abs().max((c.dy - best.dy).abs()) >= 3)
        .map(|c| c.score)
        .max_by(f64::total_cmp);
    let margin = second.map(|s| best.score - s);
    let (ma, mb) = best.stats.means();
    let (sa, sb) = best.stats.stddevs();
    let contrast = sb / sa.max(1e-9);
    let error = best.stats.absolute_error / best.stats.n as f64;
    estimate.similarity = Some(best.score);
    estimate.second_peak_similarity = second;
    estimate.peak_margin = margin;
    estimate.compared_pixels = best.stats.n;
    estimate.mean_absolute_error = Some(error);
    estimate.mean_brightness_change = Some(mb - ma);
    estimate.contrast_ratio = Some(contrast);
    estimate.left_background_similarity = best.left.ncc();
    estimate.right_background_similarity = best.right.ncc();
    if (mb - ma).abs() > MAX_MEAN_CHANGE || !(0.80..=1.25).contains(&contrast) {
        return Ok(estimate.reject(RejectionReason::ExposureChanged));
    }
    if best.score < MIN_NCC || error > MAX_MEAN_ABSOLUTE_ERROR {
        return Ok(estimate.reject(RejectionReason::SceneChanged));
    }
    let left_texture = best.left.stddevs();
    let right_texture = best.right.stddevs();
    if best.left.n < MIN_SIDE_PIXELS
        || best.right.n < MIN_SIDE_PIXELS
        || left_texture.0 < MIN_SIDE_TEXTURE
        || left_texture.1 < MIN_SIDE_TEXTURE
        || right_texture.0 < MIN_SIDE_TEXTURE
        || right_texture.1 < MIN_SIDE_TEXTURE
    {
        return Ok(estimate.reject(RejectionReason::InsufficientBackgroundTexture));
    }
    if best.left.ncc().is_none_or(|s| s < MIN_SIDE_NCC)
        || best.right.ncc().is_none_or(|s| s < MIN_SIDE_NCC)
    {
        return Ok(estimate.reject(RejectionReason::BackgroundDisagreement));
    }
    if best.dx.abs() == RADIUS || best.dy.abs() == RADIUS {
        return Ok(estimate.reject(RejectionReason::SearchBoundary));
    }
    if margin.is_none_or(|m| m < MIN_PEAK_MARGIN) {
        return Ok(estimate.reject(RejectionReason::AmbiguousPeak));
    }
    let dx = best.dx as f64 + subpixel(&candidates, best, true);
    let dy = best.dy as f64 + subpixel(&candidates, best, false);
    estimate.decidable = true;
    estimate.dx = Some(dx);
    estimate.dy = Some(dy);
    estimate.dx_source_pixels = Some(dx * w as f64 / WIDTH as f64);
    estimate.dy_source_pixels = Some(dy * h as f64 / HEIGHT as f64);
    Ok(estimate)
}

/// Numerical-only diagnostics. This does not change estimator acceptance or
/// grant calibration authority. Face boxes are current normalized rectangles,
/// not identities; unmasked patches are candidates, not verified static objects.
pub fn diagnose_frame(jpeg: &[u8], face_boxes: &[[f64; 4]]) -> Result<serde_json::Value, String> {
    use serde_json::json;
    let (gray, width, height) = decode(jpeg)?;
    if face_boxes.len() > 32
        || face_boxes
            .iter()
            .flatten()
            .any(|n| !n.is_finite() || !(0.0..=1.0).contains(n))
    {
        return Err("Diagnostic face boxes must be bounded normalized rectangles".into());
    }
    let excluded = face_boxes
        .iter()
        .filter(|b| b[0] < b[2] && b[1] < b[3])
        .map(|b| {
            let pad_x = (b[2] - b[0]) * 0.15;
            let pad_y = (b[3] - b[1]) * 0.15;
            [
                ((b[0] - pad_x).max(0.0) * f64::from(WIDTH)).floor() as i32,
                ((b[1] - pad_y).max(0.0) * f64::from(HEIGHT)).floor() as i32,
                ((b[2] + pad_x).min(1.0) * f64::from(WIDTH)).ceil() as i32,
                ((b[3] + pad_y).min(1.0) * f64::from(HEIGHT)).ceil() as i32,
            ]
        })
        .collect::<Vec<_>>();
    let mut regions = vec![
        diagnostic_region(
            &gray,
            "fixed_combined_strips",
            &[
                [EDGE, EDGE, CENTER[0], HEIGHT as i32 - EDGE],
                [CENTER[2], EDGE, WIDTH as i32 - EDGE, HEIGHT as i32 - EDGE],
            ],
            &[],
        ),
        diagnostic_region(
            &gray,
            "fixed_left_strip",
            &[[EDGE, EDGE, CENTER[0], HEIGHT as i32 - EDGE]],
            &[],
        ),
        diagnostic_region(
            &gray,
            "fixed_right_strip",
            &[[CENTER[2], EDGE, WIDTH as i32 - EDGE, HEIGHT as i32 - EDGE]],
            &[],
        ),
        diagnostic_region(
            &gray,
            "all_interior_face_excluded",
            &[[EDGE, EDGE, WIDTH as i32 - EDGE, HEIGHT as i32 - EDGE]],
            &excluded,
        ),
    ];
    // Grid tiles expose the location and orientation of texture,
    // rather than allowing distant mean brightness differences to count as it.
    for row in 0..3 {
        for column in 0..4 {
            let left = EDGE + column * 33;
            let top = EDGE + row * 20;
            regions.push(diagnostic_region(
                &gray,
                &format!("face_excluded_tile_{row}_{column}"),
                &[[left, top, left + 33, (top + 22).min(HEIGHT as i32 - EDGE)]],
                &excluded,
            ));
        }
    }
    Ok(
        json!({"analysis_width":WIDTH,"analysis_height":HEIGHT,"source_width":width,"source_height":height,"fixed_center_mask":CENTER,"fixed_edge":EDGE,"face_count":face_boxes.len(),"expanded_face_masks_analysis_pixels":excluded,"search_radius":RADIUS,"thresholds_unchanged":true,"estimator_min_global_stddev":MIN_TEXTURE,"estimator_min_side_stddev":MIN_SIDE_TEXTURE,"estimator_min_peak_margin":MIN_PEAK_MARGIN,"regions":regions,"note":"Same-frame autocorrelation diagnoses information content only. Face-excluded tiles are not proven static background, and high NCC/self-consistency does not establish physical motion or direction."}),
    )
}

fn diagnostic_region(
    gray: &GrayImage,
    name: &str,
    rectangles: &[[i32; 4]],
    excluded: &[[i32; 4]],
) -> serde_json::Value {
    use serde_json::json;
    let inside = |x: i32, y: i32| {
        x >= EDGE
            && x < WIDTH as i32 - EDGE
            && y >= EDGE
            && y < HEIGHT as i32 - EDGE
            && rectangles
                .iter()
                .any(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3])
            && !excluded
                .iter()
                .any(|r| x >= r[0] && x < r[2] && y >= r[1] && y < r[3])
    };
    let pixel = |x: i32, y: i32| gray.as_raw()[(y * WIDTH as i32 + x) as usize];
    let (mut baseline, mut gradients, mut gx_sum, mut gy_sum, mut xx, mut yy, mut xy) = (
        Stats::default(),
        0usize,
        0.0f64,
        0.0f64,
        0.0f64,
        0.0f64,
        0.0f64,
    );
    for y in EDGE..HEIGHT as i32 - EDGE {
        for x in EDGE..WIDTH as i32 - EDGE {
            if !inside(x, y) {
                continue;
            }
            baseline.add(pixel(x, y), pixel(x, y));
            if inside(x + 1, y) && inside(x, y + 1) {
                let gx = f64::from(pixel(x + 1, y)) - f64::from(pixel(x, y));
                let gy = f64::from(pixel(x, y + 1)) - f64::from(pixel(x, y));
                gradients += 1;
                gx_sum += gx.abs();
                gy_sum += gy.abs();
                xx += gx * gx;
                yy += gy * gy;
                xy += gx * gy;
            }
        }
    }
    let count = gradients.max(1) as f64;
    xx /= count;
    yy /= count;
    xy /= count;
    let discriminant = ((xx - yy) * (xx - yy) + 4.0 * xy * xy).sqrt();
    let mut candidates = Vec::new();
    for dy in -RADIUS..=RADIUS {
        for dx in -RADIUS..=RADIUS {
            let mut stats = Stats::default();
            for y in EDGE..HEIGHT as i32 - EDGE {
                for x in EDGE..WIDTH as i32 - EDGE {
                    if inside(x, y) && inside(x + dx, y + dy) {
                        stats.add(pixel(x, y), pixel(x + dx, y + dy));
                    }
                }
            }
            if stats.n >= 64 {
                if let Some(score) = stats.ncc() {
                    candidates.push((dx, dy, score, stats.n));
                }
            }
        }
    }
    candidates.sort_by(|a, b| b.2.total_cmp(&a.2));
    let best = candidates.first().copied();
    let alternative = best
        .and_then(|b| {
            candidates
                .iter()
                .find(|c| (c.0 - b.0).abs().max((c.1 - b.1).abs()) >= 3)
        })
        .copied();
    let axis_x_alternative = candidates
        .iter()
        .filter(|c| c.1 == 0 && c.0.abs() >= 3)
        .max_by(|a, b| a.2.total_cmp(&b.2));
    let axis_y_alternative = candidates
        .iter()
        .filter(|c| c.0 == 0 && c.1.abs() >= 3)
        .max_by(|a, b| a.2.total_cmp(&b.2));
    let item = |c: (i32, i32, f64, usize)| json!({"dx":c.0,"dy":c.1,"ncc":c.2,"pixels":c.3});
    let neighbors = [
        (-3, 0),
        (-1, 0),
        (1, 0),
        (3, 0),
        (0, -3),
        (0, -1),
        (0, 1),
        (0, 3),
    ]
    .into_iter()
    .map(|(dx, dy)| {
        let found = candidates.iter().find(|c| c.0 == dx && c.1 == dy).copied();
        json!({"dx":dx,"dy":dy,"ncc":found.map(|c|c.2),"pixels":found.map(|c|c.3)})
    })
    .collect::<Vec<_>>();
    json!({"name":name,"rectangles_analysis_pixels":rectangles,"pixels":baseline.n,"mean":baseline.means().0,"stddev":baseline.stddevs().0,"mean_abs_gradient_x":gx_sum/count,"mean_abs_gradient_y":gy_sum/count,"gradient_tensor_min_eigenvalue":((xx+yy-discriminant)/2.0).max(0.0),"gradient_tensor_max_eigenvalue":(xx+yy+discriminant)/2.0,"best_peak":best.map(item),"separated_second_peak":alternative.map(item),"peak_margin":best.zip(alternative).map(|(b,a)|b.2-a.2),"x_axis_competitor":axis_x_alternative.copied().map(item),"y_axis_competitor":axis_y_alternative.copied().map(item),"neighbor_autocorrelations":neighbors,"top_peaks":candidates.iter().take(6).copied().map(item).collect::<Vec<_>>()})
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{Luma, codecs::jpeg::JpegEncoder};
    fn jpeg(image: &GrayImage) -> Vec<u8> {
        let mut data = Vec::new();
        JpegEncoder::new_with_quality(&mut data, 97)
            .encode_image(image)
            .unwrap();
        data
    }
    #[test]
    fn diagnostics_distinguish_distant_brightness_difference_from_local_texture() {
        let image = GrayImage::from_fn(WIDTH, HEIGHT, |x, _| {
            Luma([if x < CENTER[0] as u32 {
                30
            } else if x >= CENTER[2] as u32 {
                230
            } else {
                128
            }])
        });
        let result = diagnose_frame(&jpeg(&image), &[]).unwrap();
        let regions = result["regions"].as_array().unwrap();
        let region = |name: &str| regions.iter().find(|r| r["name"] == name).unwrap();
        assert_eq!(region("fixed_combined_strips")["pixels"], 3720);
        assert!(region("fixed_combined_strips")["stddev"].as_f64().unwrap() > 90.0);
        assert!(region("fixed_left_strip")["stddev"].as_f64().unwrap() < 1.0);
        assert!(region("fixed_right_strip")["stddev"].as_f64().unwrap() < 1.0);
        assert_eq!(result["thresholds_unchanged"], true);
        assert!(diagnose_frame(&jpeg(&image), &[[f64::NAN, 0.0, 1.0, 1.0]]).is_err());
    }
    fn texture(seed: u64) -> GrayImage {
        let mut s = seed;
        GrayImage::from_fn(WIDTH, HEIGHT, |_, _| {
            s ^= s << 13;
            s ^= s >> 7;
            s ^= s << 17;
            Luma([40 + (s % 176) as u8])
        })
    }
    #[test]
    fn dynamic_plan_tracks_translation_with_two_fixed_nonoverlapping_patches() {
        let before = texture(0x9137);
        let prepared = prepare_background(&jpeg(&before), &jpeg(&before), &[], &[]).unwrap();
        let plan = prepared.plan.expect("two background patches");
        assert_eq!(plan.regions.len(), 2);
        assert!(!overlap(plan.regions[0], plan.regions[1]));
        let after = shifted(&before, 2, -1);
        let actual =
            estimate_background_motion(&plan, &jpeg(&before), &jpeg(&after), &[], &[]).unwrap();
        assert!(actual.motion.decidable, "{actual:?}");
        assert!((actual.motion.dx.unwrap() - 2.0).abs() < 0.25);
        assert!((actual.motion.dy.unwrap() + 1.0).abs() < 0.25);
        assert!(
            actual
                .patches
                .iter()
                .all(|p| p.peak_margin.unwrap() >= MIN_PEAK_MARGIN)
        );
    }
    #[test]
    fn multiscale_finds_two_sparse_regions_without_lowering_information_gates() {
        let mut image = GrayImage::from_pixel(WIDTH, HEIGHT, Luma([128]));
        let random = texture(0x9271);
        for region in [[14, 14, 42, 42], [14, 48, 42, 76]] {
            for y in region[1]..region[3] {
                for x in region[0]..region[2] {
                    image.put_pixel(x, y, *random.get_pixel(x, y));
                }
            }
        }
        // This expanded face mask begins at x=42. The remaining textured strip
        // is wide enough for square patches, but every old 40px window overlaps it.
        let faces = [[0.37, 0.0, 1.0, 1.0]];
        let data = jpeg(&image);
        let prepared = prepare_background(&data, &data, &faces, &faces).unwrap();
        let plan = prepared
            .plan
            .expect("two sparse independent square patches");
        assert!(prepared.quality.candidate_count > 36);
        assert_eq!(plan.regions.len(), 2);
        assert!(!overlap(plan.regions[0], plan.regions[1]));
        assert!(plan.regions.iter().all(|region| region[2] - region[0] < 40));
        assert!(prepared.quality.motion.compared_pixels >= MIN_PIXELS);
        for patch in &prepared.quality.patches {
            assert!(patch.pixels >= MIN_SIDE_PIXELS);
            assert!(patch.similarity.unwrap() >= MIN_NCC);
            assert!(patch.peak_margin.unwrap() >= MIN_PEAK_MARGIN);
            assert!(patch.before_gradient_min_eigenvalue >= MIN_GRADIENT_EIGENVALUE);
            assert!(patch.after_gradient_min_eigenvalue >= MIN_GRADIENT_EIGENVALUE);
            assert!(patch.dx.unwrap().abs() <= MAX_STATIC_SHIFT);
            assert!(patch.dy.unwrap().abs() <= MAX_STATIC_SHIFT);
            assert!(
                !face_masks(&faces)
                    .unwrap()
                    .iter()
                    .any(|mask| overlap(*mask, patch.region))
            );
        }
        let fixed = estimate_background_motion(&plan, &data, &data, &faces, &faces).unwrap();
        assert!(fixed.motion.decidable);
        let mut entered = faces.to_vec();
        entered.push([14.0 / 160.0, 14.0 / 90.0, 42.0 / 160.0, 42.0 / 90.0]);
        let rejected = estimate_background_motion(&plan, &data, &data, &faces, &entered).unwrap();
        assert!(!rejected.motion.decidable);
        assert!(
            rejected.patches.iter().any(
                |patch| patch.rejection_reason == Some(RejectionReason::FaceOverlapsBackground)
            )
        );
    }

    #[test]
    fn successful_coarse_plan_stays_preferred_and_small_pair_cannot_evade_pixel_budget() {
        let data = jpeg(&texture(0x7771));
        let prepared = prepare_background(&data, &data, &[], &[]).unwrap();
        assert_eq!(prepared.quality.candidate_count, 36);
        assert!(
            prepared
                .plan
                .unwrap()
                .regions
                .iter()
                .all(|region| region[2] - region[0] == 40 && region[3] - region[1] == 24)
        );
        let plan = BackgroundPlan {
            source_width: WIDTH,
            source_height: HEIGHT,
            regions: vec![[14, 14, 38, 38], [90, 50, 114, 74]],
            preflight_face_masks: vec![],
        };
        let insufficient = estimate_background_motion(&plan, &data, &data, &[], &[]).unwrap();
        assert!(
            insufficient
                .patches
                .iter()
                .all(|patch| patch.decidable && patch.pixels >= MIN_SIDE_PIXELS)
        );
        assert!(!insufficient.motion.decidable);
        assert_eq!(
            insufficient.motion.rejection_reason,
            Some(RejectionReason::NoIndependentBackgroundPatches)
        );
    }
    #[test]
    fn static_or_moving_face_texture_cannot_supply_background_patches() {
        let face = [40, 20, 80, 60];
        let normalized = |r: [i32; 4]| {
            [
                r[0] as f64 / WIDTH as f64,
                r[1] as f64 / HEIGHT as f64,
                r[2] as f64 / WIDTH as f64,
                r[3] as f64 / HEIGHT as f64,
            ]
        };
        let paint = |r: [i32; 4], seed: u64| {
            let mut image = GrayImage::from_pixel(WIDTH, HEIGHT, Luma([128]));
            let random = texture(seed);
            for y in r[1]..r[3] {
                for x in r[0]..r[2] {
                    image.put_pixel(x as u32, y as u32, *random.get_pixel(x as u32, y as u32));
                }
            }
            image
        };
        let before = paint(face, 17);
        assert!(
            prepare_background(
                &jpeg(&before),
                &jpeg(&before),
                &[normalized(face)],
                &[normalized(face)]
            )
            .unwrap()
            .plan
            .is_none()
        );
        let moved = [48, 20, 88, 60];
        let after = paint(moved, 19);
        assert!(
            prepare_background(
                &jpeg(&before),
                &jpeg(&after),
                &[normalized(face)],
                &[normalized(moved)]
            )
            .unwrap()
            .plan
            .is_none()
        );
    }
    #[test]
    fn moving_masked_face_does_not_turn_static_background_into_camera_motion() {
        let mut before = texture(21);
        let mut after = before.clone();
        let face = [52, 28, 84, 56];
        for y in face[1]..face[3] {
            for x in face[0]..face[2] {
                before.put_pixel(x, y, Luma([30]));
                after.put_pixel(x, y, Luma([230]));
            }
        }
        let bounds = [52.0 / 160.0, 28.0 / 90.0, 84.0 / 160.0, 56.0 / 90.0];
        let prepared =
            prepare_background(&jpeg(&before), &jpeg(&after), &[bounds], &[bounds]).unwrap();
        let plan = prepared
            .plan
            .expect("background remains usable outside face");
        let actual =
            estimate_background_motion(&plan, &jpeg(&before), &jpeg(&after), &[bounds], &[bounds])
                .unwrap();
        assert!(actual.motion.decidable);
        assert!(
            actual.motion.dx.unwrap().abs() < MAX_STATIC_SHIFT
                && actual.motion.dy.unwrap().abs() < MAX_STATIC_SHIFT
        );
    }
    #[test]
    fn repeating_texture_single_edge_single_patch_and_flat_background_reject() {
        let repeating = GrayImage::from_fn(WIDTH, HEIGHT, |x, y| {
            Luma([if ((x / 2) + (y / 2)) % 2 == 0 {
                30
            } else {
                220
            }])
        });
        let edge = GrayImage::from_fn(WIDTH, HEIGHT, |x, _| Luma([if x < 80 { 30 } else { 220 }]));
        let flat = GrayImage::from_pixel(WIDTH, HEIGHT, Luma([128]));
        let mut single = flat.clone();
        let random = texture(29);
        for y in 20..38 {
            for x in 22..58 {
                single.put_pixel(x, y, *random.get_pixel(x, y));
            }
        }
        for image in [repeating, edge, single, flat] {
            let prepared = prepare_background(&jpeg(&image), &jpeg(&image), &[], &[]).unwrap();
            assert!(
                prepared.plan.is_none(),
                "single/ambiguous patch must not establish consensus: {:?}",
                prepared.quality
            );
            assert!(!prepared.quality.motion.decidable);
        }
    }
    #[test]
    fn two_individually_valid_opposite_patch_motions_reject_consensus() {
        let before = texture(39);
        let mut after = before.clone();
        let regions = [[20, 20, 60, 44], [90, 45, 130, 69]];
        for (region, dx) in [(regions[0], 2), (regions[1], -2)] {
            for y in region[1] - RADIUS..region[3] + RADIUS {
                for x in region[0] - RADIUS..region[2] + RADIUS {
                    if x >= 0
                        && x < WIDTH as i32
                        && y >= 0
                        && y < HEIGHT as i32
                        && x - dx >= 0
                        && x - dx < WIDTH as i32
                    {
                        after.put_pixel(
                            x as u32,
                            y as u32,
                            *before.get_pixel((x - dx) as u32, y as u32),
                        );
                    }
                }
            }
        }
        let plan = BackgroundPlan {
            source_width: WIDTH,
            source_height: HEIGHT,
            regions: regions.to_vec(),
            preflight_face_masks: vec![],
        };
        let actual =
            estimate_background_motion(&plan, &jpeg(&before), &jpeg(&after), &[], &[]).unwrap();
        assert!(
            actual.patches.iter().all(|p| p.decidable),
            "individual pieces should match: {:?}",
            actual.patches
        );
        assert!(!actual.motion.decidable);
        assert_eq!(
            actual.motion.rejection_reason,
            Some(RejectionReason::PatchMotionDisagreement)
        );
    }
    #[test]
    fn entering_face_rejects_fixed_plan_without_reselecting_other_texture() {
        let image = texture(59);
        let data = jpeg(&image);
        let plan = prepare_background(&data, &data, &[], &[])
            .unwrap()
            .plan
            .unwrap();
        let r = plan.regions[0];
        let face = [
            r[0] as f64 / WIDTH as f64,
            r[1] as f64 / HEIGHT as f64,
            r[2] as f64 / WIDTH as f64,
            r[3] as f64 / HEIGHT as f64,
        ];
        let actual = estimate_background_motion(&plan, &data, &data, &[], &[face]).unwrap();
        assert!(!actual.motion.decidable);
        assert!(
            actual
                .patches
                .iter()
                .any(|p| p.rejection_reason == Some(RejectionReason::FaceOverlapsBackground))
        );
        assert_eq!(
            actual.patches.iter().map(|p| p.region).collect::<Vec<_>>(),
            plan.regions
        );
    }
    #[test]
    fn preflight_rejects_already_moving_scene_and_invalid_single_roi_authority() {
        let before = texture(79);
        let moving = shifted(&before, 1, 0);
        assert!(
            prepare_background(&jpeg(&before), &jpeg(&moving), &[], &[])
                .unwrap()
                .plan
                .is_none()
        );
        let invalid = BackgroundPlan {
            source_width: WIDTH,
            source_height: HEIGHT,
            regions: vec![[20, 20, 60, 44]],
            preflight_face_masks: vec![],
        };
        assert!(
            estimate_background_motion(&invalid, &jpeg(&before), &jpeg(&before), &[], &[]).is_err()
        );
    }
    fn shifted(before: &GrayImage, dx: i32, dy: i32) -> GrayImage {
        let mut after = GrayImage::from_pixel(WIDTH, HEIGHT, Luma([128]));
        for (x, y, pixel) in before.enumerate_pixels() {
            let (ax, ay) = (x as i32 + dx, y as i32 + dy);
            if ax >= 0 && ay >= 0 && ax < WIDTH as i32 && ay < HEIGHT as i32 {
                after.put_pixel(ax as u32, ay as u32, *pixel)
            }
        }
        after
    }
    #[test]
    fn translated_texture_has_documented_direction() {
        let before = texture(921);
        for (dx, dy) in [(4, -3), (-5, 2), (0, 0)] {
            let after = shifted(&before, dx, dy);
            let found = estimate_motion(&jpeg(&before), &jpeg(&after)).unwrap();
            assert!(found.decidable, "{found:?}");
            assert!((found.dx.unwrap() - dx as f64).abs() < 0.15, "{found:?}");
            assert!((found.dy.unwrap() - dy as f64).abs() < 0.15, "{found:?}");
            assert!(found.similarity.unwrap() > 0.98);
        }
    }
    #[test]
    fn flat_images_reject_instead_of_claiming_zero_motion() {
        let image = jpeg(&GrayImage::from_pixel(WIDTH, HEIGHT, Luma([125])));
        let found = estimate_motion(&image, &image).unwrap();
        assert!(!found.decidable);
        assert_eq!(
            found.rejection_reason,
            Some(RejectionReason::InsufficientBackgroundTexture)
        );
        assert!(found.dx.is_none() && found.dy.is_none());
    }
    #[test]
    fn large_exposure_change_is_not_motion_evidence() {
        let before = texture(547);
        let mut after = shifted(&before, 3, 1);
        for pixel in after.pixels_mut() {
            pixel[0] = pixel[0].saturating_add(45)
        }
        let found = estimate_motion(&jpeg(&before), &jpeg(&after)).unwrap();
        assert!(!found.decidable);
        assert_eq!(
            found.rejection_reason,
            Some(RejectionReason::ExposureChanged)
        );
        assert!(found.dx.is_none());
    }
    #[test]
    fn different_scene_is_rejected() {
        let found = estimate_motion(&jpeg(&texture(84)), &jpeg(&texture(91999))).unwrap();
        assert!(!found.decidable);
        assert_eq!(found.rejection_reason, Some(RejectionReason::SceneChanged));
    }
    #[test]
    fn moving_center_and_edges_do_not_override_static_background() {
        let before = texture(915);
        let mut after = shifted(&before, 3, -2);
        for (x, y, p) in after.enumerate_pixels_mut() {
            if !allowed(x as i32, y as i32) {
                *p = Luma([0])
            }
        }
        let found = estimate_motion(&jpeg(&before), &jpeg(&after)).unwrap();
        assert!(found.decidable, "{found:?}");
        assert!((found.dx.unwrap() - 3.0).abs() < 0.15);
        assert!((found.dy.unwrap() + 2.0).abs() < 0.15);
    }
    #[test]
    fn repeating_texture_rejects_ambiguous_peaks() {
        let before = GrayImage::from_fn(WIDTH, HEIGHT, |x, y| {
            Luma([if (x % 8 < 4) ^ (y % 8 < 4) { 50 } else { 205 }])
        });
        let after = shifted(&before, 2, 1);
        let found = estimate_motion(&jpeg(&before), &jpeg(&after)).unwrap();
        assert!(!found.decidable, "{found:?}");
        assert!(matches!(
            found.rejection_reason,
            Some(RejectionReason::AmbiguousPeak | RejectionReason::SearchBoundary)
        ));
    }
    #[test]
    fn oversized_shift_rejects_and_bad_jpeg_is_an_error() {
        let before = texture(145);
        let found = estimate_motion(&jpeg(&before), &jpeg(&shifted(&before, 20, 0))).unwrap();
        assert!(!found.decidable);
        assert!(estimate_motion(b"not a JPEG", b"bad").is_err());
    }
}
