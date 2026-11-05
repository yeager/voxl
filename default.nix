{
  lib,
  rustPlatform,
  pkg-config,
  x264,
  libyuv,
  libopus,
  libpulseaudio,
  xorg,
  gst_all_1,
  pipewire,
  esbuild,
  typescript,
}:
rustPlatform.buildRustPackage {
  pname = "voxl";
  version = "0.3.3";

  src = lib.fileset.toSource {
    root = ./.;
    fileset = lib.fileset.unions [
      ./Cargo.toml
      ./Cargo.lock
      ./build.rs
      ./src
      ./web
    ];
  };

  cargoLock.lockFile = ./Cargo.lock;

  nativeBuildInputs = [
    pkg-config
    rustPlatform.bindgenHook
    esbuild
    typescript
  ];

  # build.rs bundles web/src with esbuild; type-check it first.
  preBuild = ''
    tsc -p web
    tsc -p web/tsconfig.worker.json
    tsc -p web/tsconfig.worklet.json
  '';

  buildInputs = [
    x264
    libyuv
    libopus
    libpulseaudio
    xorg.libxcb
    gst_all_1.gstreamer
    gst_all_1.gst-plugins-base
    gst_all_1.gst-plugins-good
    pipewire
  ];

  meta = {
    description = "Low-latency Linux X11/Wayland desktop streaming server (H.264 over WebSocket)";
    license = lib.licenses.mit;
    mainProgram = "voxl";
    platforms = ["x86_64-linux"];
  };
}
