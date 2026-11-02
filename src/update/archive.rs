use std::io::{Cursor, Read};

use flate2::read::GzDecoder;

use super::error::UpdateError;
use super::target::Format;

fn bad(reason: impl ToString) -> UpdateError {
    UpdateError::BadArchive {
        reason: reason.to_string(),
    }
}

fn read_limited(reader: &mut impl Read, limit: u64) -> Result<Vec<u8>, UpdateError> {
    let mut out = Vec::new();
    reader.take(limit + 1).read_to_end(&mut out).map_err(bad)?;
    if out.len() as u64 > limit {
        return Err(UpdateError::TooLarge {
            what: "the executable inside the archive".to_string(),
            limit,
        });
    }
    Ok(out)
}

fn file_name(path: &std::path::Path) -> Option<&str> {
    path.file_name().and_then(|name| name.to_str())
}

fn from_zip(bytes: &[u8], exe: &str, limit: u64) -> Result<Vec<u8>, UpdateError> {
    let mut archive = zip::ZipArchive::new(Cursor::new(bytes)).map_err(bad)?;
    for index in 0..archive.len() {
        let mut entry = archive.by_index(index).map_err(bad)?;
        let matches = entry.is_file()
            && entry
                .enclosed_name()
                .is_some_and(|path| file_name(&path) == Some(exe));
        if matches {
            return read_limited(&mut entry, limit);
        }
    }
    Err(bad(format!("it has no {exe}")))
}

fn from_tar_gz(bytes: &[u8], exe: &str, limit: u64) -> Result<Vec<u8>, UpdateError> {
    let mut archive = tar::Archive::new(GzDecoder::new(bytes));
    for entry in archive.entries().map_err(bad)? {
        let mut entry = entry.map_err(bad)?;
        let path = entry.path().map_err(bad)?.into_owned();
        let safe = path
            .components()
            .all(|part| !matches!(part, std::path::Component::ParentDir));
        if entry.header().entry_type().is_file() && safe && file_name(&path) == Some(exe) {
            return read_limited(&mut entry, limit);
        }
    }
    Err(bad(format!("it has no {exe}")))
}

pub fn extract(
    format: Format,
    bytes: &[u8],
    exe: &str,
    limit: u64,
) -> Result<Vec<u8>, UpdateError> {
    let found = match format {
        Format::Zip => from_zip(bytes, exe, limit)?,
        Format::TarGz => from_tar_gz(bytes, exe, limit)?,
    };
    if found.is_empty() {
        return Err(bad(format!("{exe} is empty")));
    }
    Ok(found)
}

#[cfg(test)]
mod tests {
    use std::io::Write;

    use super::*;

    pub fn zip_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(&mut out);
        for (name, body) in files {
            writer
                .start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            writer.write_all(body).unwrap();
        }
        writer.finish().unwrap();
        out.into_inner()
    }

    pub fn tar_gz_of(files: &[(&str, &[u8])]) -> Vec<u8> {
        let mut builder = tar::Builder::new(Vec::new());
        for (name, body) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(body.len() as u64);
            header.set_mode(0o755);
            header.set_cksum();
            builder.append_data(&mut header, name, *body).unwrap();
        }
        let tar = builder.into_inner().unwrap();
        let mut gz = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::fast());
        gz.write_all(&tar).unwrap();
        gz.finish().unwrap()
    }

    #[test]
    fn finds_the_executable_in_a_zip_even_in_a_folder() {
        let zip = zip_of(&[("pkg/README.md", b"hi"), ("pkg/pipetrain.exe", b"BIN")]);
        assert_eq!(
            extract(Format::Zip, &zip, "pipetrain.exe", 1024).unwrap(),
            b"BIN"
        );
    }

    #[test]
    fn finds_the_executable_in_a_tar_gz() {
        let tgz = tar_gz_of(&[("LICENSE", b"mit"), ("pipetrain", b"BIN")]);
        assert_eq!(
            extract(Format::TarGz, &tgz, "pipetrain", 1024).unwrap(),
            b"BIN"
        );
    }

    #[test]
    fn a_missing_executable_is_a_bad_archive() {
        let tgz = tar_gz_of(&[("LICENSE", b"mit")]);
        let error = extract(Format::TarGz, &tgz, "pipetrain", 1024).unwrap_err();
        assert!(matches!(error, UpdateError::BadArchive { .. }), "{error:?}");
    }

    #[test]
    fn garbage_is_a_bad_archive() {
        for format in [Format::Zip, Format::TarGz] {
            let error = extract(format, b"not an archive", "pipetrain", 1024).unwrap_err();
            assert!(matches!(error, UpdateError::BadArchive { .. }), "{error:?}");
        }
    }

    #[test]
    fn an_oversized_executable_is_refused() {
        let zip = zip_of(&[("pipetrain.exe", &[7u8; 64])]);
        let error = extract(Format::Zip, &zip, "pipetrain.exe", 16).unwrap_err();
        assert!(matches!(error, UpdateError::TooLarge { .. }), "{error:?}");
    }
}
