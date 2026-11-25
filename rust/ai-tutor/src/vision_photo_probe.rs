use super::*;

#[test]
#[ignore = "Explicit offline probe using a separately downloaded, attributed photo"]
fn annotated_internet_photo_face_detection_probe() -> Result<(), Box<dyn std::error::Error>> {
    let input = std::env::var_os("AI_TUTOR_FACE_PROBE_IMAGE")
        .ok_or("AI_TUTOR_FACE_PROBE_IMAGE is required")?;
    let output = std::env::var_os("AI_TUTOR_FACE_PROBE_OUTPUT")
        .ok_or("AI_TUTOR_FACE_PROBE_OUTPUT is required")?;
    let report = std::env::var_os("AI_TUTOR_FACE_PROBE_REPORT")
        .ok_or("AI_TUTOR_FACE_PROBE_REPORT is required")?;
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let mut image = image::open(PathBuf::from(input))?.to_rgb8();
    let mut detector = Detector::new(&root)?;
    let faces = detector.detect(&image)?;
    assert!(!faces.is_empty(), "The reference portrait must contain a detected face");

    fn line(image: &mut RgbImage, x0: i32, y0: i32, x1: i32, y1: i32) {
        let width = image.width() as i32;
        let height = image.height() as i32;
        for x in x0.min(x1)..=x0.max(x1) {
            for y in y0.min(y1)..=y0.max(y1) {
                if x >= 0 && y >= 0 && x < width && y < height {
                    image.put_pixel(x as u32, y as u32, image::Rgb([218, 126, 98]));
                }
            }
        }
    }

    let width = image.width() as f32;
    let height = image.height() as f32;
    for face in &faces {
        let [x0, y0, x1, y1] = face.bbox;
        let left = (x0 * width) as i32;
        let top = (y0 * height) as i32;
        let right = (x1 * width) as i32;
        let bottom = (y1 * height) as i32;
        for offset in 0..6 {
            line(&mut image, left + offset, top + offset, right - offset, top + offset);
            line(&mut image, left + offset, bottom - offset, right - offset, bottom - offset);
            line(&mut image, left + offset, top + offset, left + offset, bottom - offset);
            line(&mut image, right - offset, top + offset, right - offset, bottom - offset);
        }
    }

    let output = PathBuf::from(output);
    let report = PathBuf::from(report);
    if let Some(parent) = output.parent() {
        fs::create_dir_all(parent)?;
    }
    if let Some(parent) = report.parent() {
        fs::create_dir_all(parent)?;
    }
    image.save(&output)?;
    let result = json!({
        "schema_version": 1,
        "source": "https://unsplash.com/photos/mEZ3PoFGs_k",
        "photographer": "Michael Dam",
        "license": "Unsplash License",
        "operation": "offline_face_detection",
        "identity_recognition": false,
        "emotion_inference": false,
        "raw_media_sent_to_remote": false,
        "image": {"width": image.width(), "height": image.height()},
        "face_count": faces.len(),
        "faces": faces.iter().map(|face| json!({"score": face.score, "bbox_normalized": face.bbox})).collect::<Vec<_>>()
    });
    fs::write(&report, serde_json::to_vec_pretty(&result)?)?;
    println!("{result}");
    Ok(())
}
