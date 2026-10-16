# Files: uploads and downloads

## Uploading

`act(action="upload", ref=…, files=["/abs/path/report.pdf"])` puts local files into a
file input. The ref can be the `<input type=file>` itself or the button or link that
opens a file chooser ("Upload", "Attach", "Choose file"): the chooser is answered
without a native dialog.

- Paths must be absolute, regular, readable files. Hidden files and folders (`~/.ssh`,
  `.env`) and key stores (`id_rsa`, `.pem`, `.p12`, `.kdbx`, keychains) are refused,
  whatever the path says through symlinks.
- An upload always needs approval: the user's in an attended session (the approval
  names each file and its size, and the site), or `confirm=true` in an unattended one
  when your instructions allow it. At most 20 files at once.
- After the upload, submit the form as a separate step; that step is judged like any
  other.

## Downloading

Files the page downloads land in `~/.aibrowse/downloads/<profile>/`, named as the site
names them (never overwriting: `report (1).pdf`).

- The outline marks links that download: `(downloads a file)`.
- Any action that starts a download says so (name, size, done or in progress).
- `downloads()` lists the session's downloads with their paths; `downloads(wait_seconds=30)`
  waits for ones still in progress.
- PDFs opened in the browser are read as text by `read`; you don't need to download
  them to read them.
