//! Folders picked in the desktop's file dialog, through xdg-desktop-portal's
//! FileChooser rather than a dialog of the Agent's own. In a machine, such
//! as the one LosOS runs the Agent in, the portal is the way to the person's
//! files: their dialog runs outside it, and what they pick is handed to the
//! machine as a copy whose changes go back. A typed path names only what is
//! already in here.

use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;
use std::path::PathBuf;

use ashpd::desktop::ResponseError;
use ashpd::desktop::file_chooser::SelectedFiles;

/// Asks for one folder, blocking until the person picks one or cancels,
/// which is `Ok(None)`. Off the UI thread: the dialog is another process's.
pub fn choose_folder(title: &str) -> Result<Option<PathBuf>, String> {
    // zbus runs on tokio, as in derisk's portal; one small runtime per
    // request, since the Agent has no other use for one.
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("could not start the portal request: {error}"))?;
    let response = runtime.block_on(async {
        SelectedFiles::open_file()
            .title(title)
            .accept_label("Add")
            .modal(true)
            .directory(true)
            .send()
            .await?
            .response()
    });
    match response {
        Ok(files) => files
            .uris()
            .first()
            .map(|uri| {
                path_from_uri(uri.as_str())
                    .ok_or_else(|| format!("the dialog gave a folder that is not a file: {uri}"))
            })
            .transpose(),
        Err(ashpd::Error::Response(ResponseError::Cancelled)) => Ok(None),
        Err(error) => Err(format!("no file dialog: {error}")),
    }
}

/// The path of a local `file://` URI, percent-decoded; none for any other.
fn path_from_uri(uri: &str) -> Option<PathBuf> {
    let rest = uri.strip_prefix("file://")?;
    // A host part, which portals leave empty, is not a path of this machine.
    let path = rest.strip_prefix("localhost").unwrap_or(rest);
    if !path.starts_with('/') {
        return None;
    }
    let mut bytes = Vec::with_capacity(path.len());
    let mut input = path.bytes();
    while let Some(byte) = input.next() {
        if byte == b'%' {
            let high = (input.next()? as char).to_digit(16)?;
            let low = (input.next()? as char).to_digit(16)?;
            bytes.push((high * 16 + low) as u8);
        } else {
            bytes.push(byte);
        }
    }
    Some(PathBuf::from(OsString::from_vec(bytes)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_uris_become_paths() {
        assert_eq!(
            path_from_uri("file:///home/user/.local/share/losos-portal/3/My%20repo"),
            Some(PathBuf::from(
                "/home/user/.local/share/losos-portal/3/My repo"
            ))
        );
        assert_eq!(
            path_from_uri("file://localhost/srv/a%2Fb"),
            Some(PathBuf::from("/srv/a/b"))
        );
        assert_eq!(path_from_uri("file://host/srv"), None);
        assert_eq!(path_from_uri("https://example.com/"), None);
        assert_eq!(path_from_uri("file:///bad%2"), None);
        assert_eq!(path_from_uri("file:///bad%zz"), None);
    }
}
