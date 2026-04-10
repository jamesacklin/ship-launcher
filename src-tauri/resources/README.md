# Resources

Place pier archives here for bundling into the app. For example:

```
resources/
├── sampel-palnet.tar.gz
└── pier-manifest.json
```

Then add the following to `bundle` in `tauri.conf.json`:

```json
"resources": [
  "resources/sampel-palnet.tar.gz",
  "resources/pier-manifest.json"
]
```

These files are gitignored — CI generates them per-build.
