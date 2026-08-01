//! 768px content-addressed JPEG thumbnails (PRD §7).
//!
//! Layout: `thumbs/<sha[0:2]>/<sha[2:4]>/<sha>.jpg`, where `sha` is the SHA-256
//! of the **encoded thumbnail bytes**. Content addressing means two uploads of
//! the same photo collapse to one file, and the path is derivable from the hash
//! alone — no directory scan is ever needed.
//!
//! Two years at 5 meals/day is roughly 350MB, hence the 5Gi PVC.

use crate::error::AppError;
use crate::models::{NewThumbnail, Thumbnail};
use diesel::prelude::*;
use diesel::SqliteConnection;
use image::{DynamicImage, ImageDecoder, ImageReader, Limits};
use sha2::{Digest, Sha256};
use std::io::Cursor;
use std::path::{Path, PathBuf};

/// Long-edge target in pixels. Vision models downscale to about this range
/// anyway, so the display asset and the re-analysis input are the same file.
pub const LONG_EDGE: u32 = 768;

/// JPEG quality for the derivative — ~60–100KB at 768px.
pub const JPEG_QUALITY: u8 = 75;

/// Largest upload accepted, before decoding. Guards the multipart handler.
pub const MAX_UPLOAD_BYTES: usize = 32 * 1024 * 1024;

/// Widest/tallest source image the decoder will accept.
///
/// A 32MiB upload can still describe a 60,000×60,000 image; without a strict
/// dimension limit that is a decompression bomb rather than a photo.
pub const MAX_SOURCE_EDGE: u32 = 20_000;

/// Ceiling on decoder allocations for one upload.
pub const MAX_DECODE_ALLOC: u64 = 256 * 1024 * 1024;

/// Resampling filter used for the downscale. Lanczos3 costs a few milliseconds
/// more than Triangle and preserves the fine texture a vision model reads
/// portion size from.
const RESIZE_FILTER: image::imageops::FilterType = image::imageops::FilterType::Lanczos3;

/// MIME type of every stored derivative.
pub const CONTENT_TYPE: &str = "image/jpeg";

/// A thumbnail that has been encoded and written to disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredThumb {
    /// SHA-256 of the encoded JPEG, lowercase hex.
    pub sha256: String,
    /// Path relative to the thumbs root, e.g. `ab/cd/abcd….jpg`.
    pub rel_path: String,
    /// Width of the derivative in pixels.
    pub width: i32,
    /// Height of the derivative in pixels.
    pub height: i32,
    /// Size of the encoded JPEG in bytes.
    pub bytes: i64,
}

/// Decode `data`, scale its long edge down to [`LONG_EDGE`], re-encode as JPEG
/// and write it under `root`.
///
/// Images already at or below the target size are re-encoded but not upscaled.
/// Writing is idempotent: an existing file with the same hash is left alone.
///
/// EXIF orientation is applied before scaling. Phone cameras routinely record a
/// landscape sensor read plus a rotation tag; ignoring it would hand the vision
/// model a sideways plate and show the user a sideways thumbnail.
pub fn store_from_bytes(root: &Path, data: &[u8]) -> Result<StoredThumb, AppError> {
    let encoded = encode_thumbnail(data)?;
    let sha256 = sha256_hex(&encoded.bytes);
    let rel_path = relative_path_for(&sha256)?;
    let path = absolute_path(root, &rel_path)?;

    // Content-addressed: identical bytes already on disk are already correct,
    // so a re-upload of the same photo is a no-op rather than a rewrite.
    let already_present = std::fs::metadata(&path)
        .map(|meta| meta.len() == encoded.bytes.len() as u64)
        .unwrap_or(false);
    if !already_present {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        write_atomically(&path, &encoded.bytes)?;
    }

    Ok(StoredThumb {
        sha256,
        rel_path,
        width: encoded.width as i32,
        height: encoded.height as i32,
        bytes: encoded.bytes.len() as i64,
    })
}

/// An encoded derivative, before it has a home on disk.
struct EncodedThumb {
    bytes: Vec<u8>,
    width: u32,
    height: u32,
}

/// Decode, orient, downscale and JPEG-encode one upload.
///
/// Split out from [`store_from_bytes`] so the image pipeline is testable without
/// touching the filesystem.
fn encode_thumbnail(data: &[u8]) -> Result<EncodedThumb, AppError> {
    if data.is_empty() {
        return Err(AppError::BadRequest("the uploaded image is empty".into()));
    }
    if data.len() > MAX_UPLOAD_BYTES {
        return Err(AppError::BadRequest(format!(
            "image is {} bytes; the limit is {MAX_UPLOAD_BYTES}",
            data.len()
        )));
    }

    let mut reader = ImageReader::new(Cursor::new(data))
        .with_guessed_format()
        .map_err(|e| AppError::BadRequest(format!("unrecognized image data: {e}")))?;
    let mut limits = Limits::default();
    limits.max_image_width = Some(MAX_SOURCE_EDGE);
    limits.max_image_height = Some(MAX_SOURCE_EDGE);
    limits.max_alloc = Some(MAX_DECODE_ALLOC);
    reader.limits(limits);

    let mut decoder = reader.into_decoder()?;
    // An unreadable orientation tag is not a reason to reject a photo.
    let orientation = decoder
        .orientation()
        .unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut image = DynamicImage::from_decoder(decoder)?;
    image.apply_orientation(orientation);

    let (width, height) = (image.width(), image.height());
    if width == 0 || height == 0 {
        return Err(AppError::BadRequest("the image has no pixels".into()));
    }
    // `DynamicImage::resize` fits within the box but also *upscales*; a photo
    // already below the target must not be blown up to 768.
    if width.max(height) > LONG_EDGE {
        image = image.resize(LONG_EDGE, LONG_EDGE, RESIZE_FILTER);
    }

    // Drop the alpha channel: JPEG has none, and encoding RGBA would either
    // fail or silently composite against an arbitrary background.
    let rgb = image.to_rgb8();
    let (width, height) = (rgb.width(), rgb.height());

    let mut bytes = Vec::with_capacity(128 * 1024);
    image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, JPEG_QUALITY)
        .encode_image(&rgb)?;

    Ok(EncodedThumb {
        bytes,
        width,
        height,
    })
}

/// Write via a temp file in the same directory, then rename.
///
/// A torn write would poison the content address permanently: the hash says the
/// bytes are correct while the file on disk is half a photo.
fn write_atomically(path: &Path, bytes: &[u8]) -> Result<(), AppError> {
    let tmp = path.with_extension(format!("tmp{}", uuid::Uuid::new_v4().simple()));
    std::fs::write(&tmp, bytes)?;
    match std::fs::rename(&tmp, path) {
        Ok(()) => Ok(()),
        Err(e) => {
            let _ = std::fs::remove_file(&tmp);
            Err(AppError::from(e))
        }
    }
}

/// Lowercase hex SHA-256 of a byte slice.
pub fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(char::from_digit((byte >> 4) as u32, 16).unwrap_or('0'));
        out.push(char::from_digit((byte & 0x0f) as u32, 16).unwrap_or('0'));
    }
    out
}

/// Path of a thumbnail relative to the thumbs root, derived from its hash.
///
/// Two levels of 2-hex-character fan-out keep any single directory small.
pub fn relative_path_for(sha256: &str) -> Result<String, AppError> {
    if sha256.len() < 4 || !sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(AppError::Internal(format!(
            "not a sha256 hex digest: {sha256:?}"
        )));
    }
    Ok(format!("{}/{}/{}.jpg", &sha256[0..2], &sha256[2..4], sha256))
}

/// Join a stored relative path onto the thumbs root.
///
/// Rejects paths that try to escape the root; `rel_path` comes from the
/// database, but a traversal here would be a filesystem read primitive.
pub fn absolute_path(root: &Path, rel_path: &str) -> Result<PathBuf, AppError> {
    if rel_path.is_empty()
        || rel_path.starts_with('/')
        || rel_path.split('/').any(|seg| seg == ".." || seg == ".")
    {
        return Err(AppError::Internal(format!(
            "refusing to resolve thumbnail path {rel_path:?}"
        )));
    }
    Ok(root.join(rel_path))
}

/// Read a stored thumbnail's bytes.
pub fn read(root: &Path, rel_path: &str) -> Result<Vec<u8>, AppError> {
    let path = absolute_path(root, rel_path)?;
    std::fs::read(&path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            AppError::NotFound(format!("thumbnail {rel_path} is missing from disk"))
        }
        _ => AppError::from(e),
    })
}

/// Read a stored thumbnail as standard base64, ready to become the `data`
/// payload of a provider image message.
///
/// PRD §5 requires images to be **re-attached from disk** on every re-analysis
/// rather than assumed to survive in serialized history, because provider
/// message formats vary on image retention. This is that read.
pub fn read_base64(root: &Path, rel_path: &str) -> Result<String, AppError> {
    Ok(encode_base64(&read(root, rel_path)?))
}

/// Read a thumbnail by absolute path as standard base64.
///
/// [`crate::agent::AnalysisContext`] carries an absolute `image_path`, which is
/// what the analysis worker resolved once; this saves re-deriving the root.
pub fn read_base64_at(path: &Path) -> Result<String, AppError> {
    let bytes = std::fs::read(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => {
            AppError::NotFound(format!("thumbnail {} is missing from disk", path.display()))
        }
        _ => AppError::from(e),
    })?;
    Ok(encode_base64(&bytes))
}

/// Standard base64 (RFC 4648 §4, with padding).
///
/// Hand-rolled deliberately: the crate graph has no direct `base64` dependency,
/// and 20 lines here is cheaper than an extra crate for one call site.
pub fn encode_base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] =
        b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        out.push(ALPHABET[(triple >> 18) as usize & 0x3f] as char);
        out.push(ALPHABET[(triple >> 12) as usize & 0x3f] as char);
        out.push(if chunk.len() > 1 {
            ALPHABET[(triple >> 6) as usize & 0x3f] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            ALPHABET[triple as usize & 0x3f] as char
        } else {
            '='
        });
    }
    out
}

/// Delete a stored thumbnail. Missing files are not an error.
pub fn delete(root: &Path, rel_path: &str) -> Result<(), AppError> {
    let path = absolute_path(root, rel_path)?;
    match std::fs::remove_file(&path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Insert (or return the existing) `thumbnails` row for a stored derivative.
///
/// `sha256` is `UNIQUE`, so a re-upload of the same photo reuses the row — that
/// is the dedup the content addressing is for. The row belongs to whoever
/// uploaded the bytes first; the second uploader gets the same row, which is
/// acceptable at 2–10 accounts and keeps one file per distinct photo.
pub fn upsert_thumbnail(
    conn: &mut SqliteConnection,
    user_id: &str,
    thumb: &StoredThumb,
) -> Result<Thumbnail, AppError> {
    use crate::schema::thumbnails::dsl;

    if let Some(existing) = find_by_sha256(conn, &thumb.sha256)? {
        return Ok(existing);
    }

    let row = new_thumbnail_row(user_id, thumb);
    match diesel::insert_into(dsl::thumbnails)
        .values(&row)
        .returning(Thumbnail::as_returning())
        .get_result::<Thumbnail>(conn)
    {
        Ok(inserted) => Ok(inserted),
        // Two uploads of the same photo can race between the select and the
        // insert; the loser reads the winner's row rather than failing.
        Err(diesel::result::Error::DatabaseError(
            diesel::result::DatabaseErrorKind::UniqueViolation,
            _,
        )) => find_by_sha256(conn, &thumb.sha256)?.ok_or_else(|| {
            AppError::Internal(format!(
                "thumbnail {} vanished between insert and re-read",
                thumb.sha256
            ))
        }),
        Err(e) => Err(AppError::from(e)),
    }
}

/// Look a thumbnail up by its content hash.
pub fn find_by_sha256(
    conn: &mut SqliteConnection,
    sha256: &str,
) -> Result<Option<Thumbnail>, AppError> {
    use crate::schema::thumbnails::dsl;
    dsl::thumbnails
        .filter(dsl::sha256.eq(sha256))
        .select(Thumbnail::as_select())
        .first::<Thumbnail>(conn)
        .optional()
        .map_err(AppError::from)
}

/// Load a thumbnail row by id.
pub fn find_by_id(conn: &mut SqliteConnection, id: &str) -> Result<Option<Thumbnail>, AppError> {
    use crate::schema::thumbnails::dsl;
    dsl::thumbnails
        .filter(dsl::id.eq(id))
        .select(Thumbnail::as_select())
        .first::<Thumbnail>(conn)
        .optional()
        .map_err(AppError::from)
}

/// Build the insert row for a freshly stored derivative.
pub fn new_thumbnail_row(user_id: &str, thumb: &StoredThumb) -> NewThumbnail {
    NewThumbnail {
        id: uuid::Uuid::new_v4().to_string(),
        user_id: user_id.to_string(),
        sha256: thumb.sha256.clone(),
        path: thumb.rel_path.clone(),
        width: thumb.width,
        height: thumb.height,
        bytes: thumb.bytes,
        created_at: chrono::Utc::now().naive_utc(),
    }
}

/// Delete any thumbnail no longer referenced by a meal.
///
/// Meals are deletable (`DELETE /api/v1/meals/:id`) and thumbnails are shared
/// by content hash, so reclaiming disk needs a reference check. Returns how many
/// rows were removed.
///
/// The file is unlinked before the row so a crash in between leaves a row
/// pointing at a missing file — recoverable on the next pass — rather than a
/// file nothing references, which nothing would ever find again.
pub fn prune_orphans(conn: &mut SqliteConnection, root: &Path) -> Result<usize, AppError> {
    use crate::schema::{meals, thumbnails};

    let orphans: Vec<(String, String)> = thumbnails::table
        .filter(diesel::dsl::not(diesel::dsl::exists(
            meals::table.filter(meals::thumbnail_id.eq(thumbnails::id.nullable())),
        )))
        .select((thumbnails::id, thumbnails::path))
        .load::<(String, String)>(conn)?;

    let mut removed = 0usize;
    for (id, rel_path) in orphans {
        if let Err(e) = delete(root, &rel_path) {
            tracing::warn!(thumbnail = %id, error = %e, "could not unlink orphaned thumbnail");
            continue;
        }
        let deleted =
            diesel::delete(thumbnails::table.filter(thumbnails::id.eq(&id))).execute(conn)?;
        removed += deleted;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::{ImageFormat, Rgb, RgbImage};

    const SHA: &str = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";

    /// A deterministic PNG of the requested size, with enough variation that
    /// JPEG cannot collapse it to a flat block.
    fn png(width: u32, height: u32) -> Vec<u8> {
        let mut img = RgbImage::new(width, height);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = Rgb([(x % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8]);
        }
        let mut bytes = Vec::new();
        DynamicImage::ImageRgb8(img)
            .write_to(&mut Cursor::new(&mut bytes), ImageFormat::Png)
            .unwrap();
        bytes
    }

    #[test]
    fn relative_path_fans_out_two_levels() {
        assert_eq!(relative_path_for(SHA).unwrap(), format!("01/23/{SHA}.jpg"));
    }

    #[test]
    fn relative_path_rejects_non_hex() {
        assert!(relative_path_for("../../etc/passwd").is_err());
    }

    #[test]
    fn absolute_path_rejects_traversal() {
        let root = Path::new("/data/thumbs");
        assert!(absolute_path(root, "../../etc/passwd").is_err());
        assert!(absolute_path(root, "/etc/passwd").is_err());
        assert!(absolute_path(root, "01/23/abc.jpg").is_ok());
    }

    #[test]
    fn the_long_edge_is_scaled_down_and_the_aspect_ratio_kept() {
        let dir = tempfile::tempdir().unwrap();
        let stored = store_from_bytes(dir.path(), &png(2000, 1000)).unwrap();
        assert_eq!(stored.width, LONG_EDGE as i32);
        assert_eq!(stored.height, (LONG_EDGE / 2) as i32);
        assert!(stored.bytes > 0);
    }

    #[test]
    fn a_portrait_photo_scales_by_its_height() {
        let dir = tempfile::tempdir().unwrap();
        let stored = store_from_bytes(dir.path(), &png(600, 1800)).unwrap();
        assert_eq!(stored.height, LONG_EDGE as i32);
        assert_eq!(stored.width, 256);
    }

    #[test]
    fn small_images_are_never_upscaled() {
        let dir = tempfile::tempdir().unwrap();
        let stored = store_from_bytes(dir.path(), &png(320, 240)).unwrap();
        assert_eq!((stored.width, stored.height), (320, 240));
    }

    #[test]
    fn the_file_lands_at_its_content_address() {
        let dir = tempfile::tempdir().unwrap();
        let stored = store_from_bytes(dir.path(), &png(1024, 768)).unwrap();
        let path = dir.path().join(&stored.rel_path);
        assert!(path.exists(), "{} should exist", path.display());
        assert_eq!(
            stored.rel_path,
            format!("{}/{}/{}.jpg", &stored.sha256[0..2], &stored.sha256[2..4], stored.sha256)
        );
        let on_disk = std::fs::read(&path).unwrap();
        assert_eq!(sha256_hex(&on_disk), stored.sha256);
        assert_eq!(on_disk.len() as i64, stored.bytes);
    }

    #[test]
    fn identical_uploads_dedup_to_one_file() {
        let dir = tempfile::tempdir().unwrap();
        let source = png(900, 900);
        let first = store_from_bytes(dir.path(), &source).unwrap();
        let second = store_from_bytes(dir.path(), &source).unwrap();
        assert_eq!(first, second);

        let files: Vec<_> = walk(dir.path());
        assert_eq!(files.len(), 1, "content addressing should collapse the two");
    }

    #[test]
    fn different_photos_get_different_addresses() {
        let dir = tempfile::tempdir().unwrap();
        let a = store_from_bytes(dir.path(), &png(800, 600)).unwrap();
        let b = store_from_bytes(dir.path(), &png(801, 600)).unwrap();
        assert_ne!(a.sha256, b.sha256);
        assert_eq!(walk(dir.path()).len(), 2);
    }

    #[test]
    fn junk_bytes_are_a_client_error_not_a_panic() {
        let dir = tempfile::tempdir().unwrap();
        let err = store_from_bytes(dir.path(), b"this is not an image at all").unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::BAD_REQUEST);
    }

    #[test]
    fn an_empty_upload_is_rejected() {
        let dir = tempfile::tempdir().unwrap();
        assert!(store_from_bytes(dir.path(), b"").is_err());
    }

    #[test]
    fn reading_a_missing_thumbnail_is_a_404_not_a_500() {
        let dir = tempfile::tempdir().unwrap();
        let err = read(dir.path(), &relative_path_for(SHA).unwrap()).unwrap_err();
        assert_eq!(err.status(), axum::http::StatusCode::NOT_FOUND);
    }

    #[test]
    fn deleting_a_missing_thumbnail_is_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        assert!(delete(dir.path(), &relative_path_for(SHA).unwrap()).is_ok());
    }

    #[test]
    fn base64_matches_rfc4648_vectors() {
        assert_eq!(encode_base64(b""), "");
        assert_eq!(encode_base64(b"f"), "Zg==");
        assert_eq!(encode_base64(b"fo"), "Zm8=");
        assert_eq!(encode_base64(b"foo"), "Zm9v");
        assert_eq!(encode_base64(b"foob"), "Zm9vYg==");
        assert_eq!(encode_base64(b"fooba"), "Zm9vYmE=");
        assert_eq!(encode_base64(b"foobar"), "Zm9vYmFy");
        assert_eq!(encode_base64(&[0xff, 0xfe, 0xfd]), "//79");
    }

    #[test]
    fn a_stored_thumbnail_round_trips_through_base64() {
        let dir = tempfile::tempdir().unwrap();
        let stored = store_from_bytes(dir.path(), &png(400, 400)).unwrap();
        let encoded = read_base64(dir.path(), &stored.rel_path).unwrap();
        assert!(
            encoded.starts_with("/9j/"),
            "JPEG magic should survive base64: {}",
            &encoded[..8]
        );
        assert_eq!(encoded.len(), (stored.bytes as usize).div_ceil(3) * 4);

        let by_path = read_base64_at(&dir.path().join(&stored.rel_path)).unwrap();
        assert_eq!(encoded, by_path);
    }

    #[test]
    fn sha256_matches_the_published_empty_digest() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
        assert_eq!(
            sha256_hex(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    // -----------------------------------------------------------------------
    // Database-backed dedup and pruning
    // -----------------------------------------------------------------------

    #[test]
    fn the_same_photo_from_two_uploads_reuses_one_row() {
        use crate::feedback::state::fixtures::*;
        use crate::schema::thumbnails::dsl;

        let dir = tempfile::tempdir().unwrap();
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");

        // 800×600 downscales to 768×576; the row records the derivative, not
        // the upload, because the derivative is all that survives (§7).
        let stored = store_from_bytes(dir.path(), &png(800, 600)).unwrap();
        let first = upsert_thumbnail(&mut conn, "u1", &stored).unwrap();
        let second = upsert_thumbnail(&mut conn, "u1", &stored).unwrap();

        assert_eq!(first.id, second.id);
        assert_eq!(first.sha256, stored.sha256);
        assert_eq!(first.path, stored.rel_path);
        assert_eq!((first.width, first.height), (768, 576));
        assert_eq!(first.bytes, stored.bytes);

        let count: i64 = dsl::thumbnails.count().get_result(&mut conn).unwrap();
        assert_eq!(count, 1);

        assert_eq!(find_by_sha256(&mut conn, &stored.sha256).unwrap().unwrap().id, first.id);
        assert_eq!(find_by_id(&mut conn, &first.id).unwrap().unwrap().sha256, stored.sha256);
        assert!(find_by_id(&mut conn, "nope").unwrap().is_none());
    }

    #[test]
    fn pruning_removes_unreferenced_thumbnails_and_leaves_the_rest() {
        use crate::feedback::state::fixtures::*;
        use crate::models::MealStatus;
        use crate::schema::{meals, thumbnails};

        let dir = tempfile::tempdir().unwrap();
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");

        let kept = upsert_thumbnail(
            &mut conn,
            "u1",
            &store_from_bytes(dir.path(), &png(400, 300)).unwrap(),
        )
        .unwrap();
        let orphan = upsert_thumbnail(
            &mut conn,
            "u1",
            &store_from_bytes(dir.path(), &png(401, 300)).unwrap(),
        )
        .unwrap();

        seed_meal(
            &mut conn,
            "m1",
            "u1",
            at(2026, 8, 1, 12, 0),
            0,
            MealStatus::Confirmed,
            1,
            1.0,
            None,
        );
        diesel::update(meals::table.filter(meals::id.eq("m1")))
            .set(meals::thumbnail_id.eq(Some(kept.id.clone())))
            .execute(&mut conn)
            .unwrap();

        assert_eq!(walk(dir.path()).len(), 2);
        assert_eq!(prune_orphans(&mut conn, dir.path()).unwrap(), 1);

        let remaining: Vec<String> = thumbnails::table
            .select(thumbnails::id)
            .load(&mut conn)
            .unwrap();
        assert_eq!(remaining, vec![kept.id]);
        assert_eq!(walk(dir.path()).len(), 1, "the orphan's file is unlinked too");
        assert!(read(dir.path(), &orphan.path).is_err());

        // Nothing left to reclaim: a second pass is a no-op.
        assert_eq!(prune_orphans(&mut conn, dir.path()).unwrap(), 0);
    }

    #[test]
    fn a_thumbnail_shared_by_two_meals_survives_losing_one() {
        use crate::feedback::state::fixtures::*;
        use crate::models::MealStatus;
        use crate::schema::meals;

        let dir = tempfile::tempdir().unwrap();
        let mut conn = test_conn();
        seed_user(&mut conn, "u1");

        let shared = upsert_thumbnail(
            &mut conn,
            "u1",
            &store_from_bytes(dir.path(), &png(500, 500)).unwrap(),
        )
        .unwrap();
        for id in ["m1", "m2"] {
            seed_meal(
                &mut conn,
                id,
                "u1",
                at(2026, 8, 1, 12, 0),
                0,
                MealStatus::Confirmed,
                1,
                1.0,
                None,
            );
            diesel::update(meals::table.filter(meals::id.eq(id)))
                .set(meals::thumbnail_id.eq(Some(shared.id.clone())))
                .execute(&mut conn)
                .unwrap();
        }

        diesel::delete(meals::table.filter(meals::id.eq("m1")))
            .execute(&mut conn)
            .unwrap();
        assert_eq!(prune_orphans(&mut conn, dir.path()).unwrap(), 0);
        assert!(find_by_id(&mut conn, &shared.id).unwrap().is_some());

        diesel::delete(meals::table.filter(meals::id.eq("m2")))
            .execute(&mut conn)
            .unwrap();
        assert_eq!(prune_orphans(&mut conn, dir.path()).unwrap(), 1);
        assert!(find_by_id(&mut conn, &shared.id).unwrap().is_none());
    }

    /// Every regular file below `root`, recursively.
    fn walk(root: &Path) -> Vec<PathBuf> {
        let mut out = Vec::new();
        let mut stack = vec![root.to_path_buf()];
        while let Some(dir) = stack.pop() {
            for entry in std::fs::read_dir(&dir).into_iter().flatten().flatten() {
                let path = entry.path();
                if path.is_dir() {
                    stack.push(path);
                } else {
                    out.push(path);
                }
            }
        }
        out
    }
}
