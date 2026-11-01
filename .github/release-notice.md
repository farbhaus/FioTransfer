### First launch

FioTransfer isn't notarized by Apple yet, so macOS blocks the first launch with "Apple could not verify…". After copying it to Applications, run this in Terminal:

```sh
xattr -dr com.apple.quarantine /Applications/FioTransfer.app
```

Or click **Done**, then **Open Anyway** in **System Settings → Privacy & Security**.

Updates installed from inside the app aren't affected.
