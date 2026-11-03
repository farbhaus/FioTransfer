//! Files dropped on the window (the webview's `OnFileDrop` → `onDrop`): an
//! upload, the way Files… and Folder… start one — to the remembered
//! destination, or through the picker with the paths waiting when there is none
//! yet; signed out, the shell says why (`ensureCanUpload`).

use crate::model::{Request, UiState, View};

/// What was dropped this frame, as an upload from the Transfers screen. An open
/// watch editor stays in front, as the webview's modal did over `showView`.
pub fn take(ctx: &egui::Context, state: &mut UiState) {
    let paths: Vec<String> = ctx.input(|i| {
        i.raw
            .dropped_files
            .iter()
            .filter_map(|file| file.path.as_ref())
            .map(|path| path.to_string_lossy().into_owned())
            .collect()
    });
    if paths.is_empty() {
        return;
    }
    if state.watch_editor.is_none() {
        state.view = View::Transfers;
    }
    state.ask(Request::StartUpload(paths));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::WatchEditor;
    use egui::DroppedFile;
    use egui_kittest::Harness;

    fn harness(state: UiState) -> Harness<'static, UiState> {
        Harness::builder()
            .with_size(egui::vec2(956.0, 700.0))
            .build_state(|ctx, state: &mut UiState| take(ctx, state), state)
    }

    fn dropped(path: &str) -> DroppedFile {
        DroppedFile { path: Some(path.into()), ..Default::default() }
    }

    #[test]
    fn a_drop_uploads_what_landed_from_the_transfers_screen() {
        let mut harness = harness(UiState { view: View::History, ..UiState::default() });
        harness.input_mut().dropped_files.push(dropped("/Volumes/Supafast/A001.mov"));
        harness.input_mut().dropped_files.push(dropped("/Volumes/Supafast/Renders"));
        harness.step();
        let state = harness.state();
        assert_eq!(state.view, View::Transfers);
        assert_eq!(
            state.requests,
            vec![Request::StartUpload(vec![
                "/Volumes/Supafast/A001.mov".into(),
                "/Volumes/Supafast/Renders".into(),
            ])]
        );
    }

    #[test]
    fn a_drop_under_the_watch_editor_leaves_it_in_front() {
        let mut harness = harness(UiState {
            view: View::Watch,
            watch_editor: Some(WatchEditor::default()),
            ..UiState::default()
        });
        harness.input_mut().dropped_files.push(dropped("/Users/me/clip.mov"));
        harness.step();
        let state = harness.state();
        assert_eq!(state.view, View::Watch);
        assert!(state.watch_editor.is_some());
        assert_eq!(state.requests, vec![Request::StartUpload(vec!["/Users/me/clip.mov".into()])]);
    }

    #[test]
    fn files_only_passing_over_the_window_upload_nothing() {
        let mut harness = harness(UiState { view: View::History, ..UiState::default() });
        harness.input_mut().hovered_files.push(egui::HoveredFile {
            path: Some("/Users/me/clip.mov".into()),
            ..Default::default()
        });
        harness.step();
        // Bytes without a path (a browser's drop) are not a file to upload either.
        harness.input_mut().hovered_files.clear();
        harness.input_mut().dropped_files.push(DroppedFile { name: "clip.mov".into(), ..Default::default() });
        harness.step();
        assert!(harness.state().requests.is_empty());
        assert_eq!(harness.state().view, View::History);
    }
}
