{
  # Reproducible NixOS / Nix build of the FileID Linux app.
  #
  #   nix build  ./packaging/nix#fileid
  #   nix run    ./packaging/nix#fileid
  #
  # Builds the GTK4 workspace (platforms/linux) plus the engine binary it
  # spawns, with gtk4 + libadwaita + onnxruntime from nixpkgs and wrapGAppsHook4
  # for the GTK/GdkPixbuf/GSettings runtime environment.
  #
  # ============================ ONNX RUNTIME — RISKIEST PART ==================
  # The engine's `ort` crate is configured `load-dynamic` + `download-binaries`
  # (platforms/windows/src/engine/Cargo.toml). The Nix build sandbox has NO
  # network, so the `download-binaries` build script CANNOT fetch onnxruntime.
  # We therefore:
  #   * put nixpkgs `onnxruntime` in buildInputs,
  #   * set ORT_LIB_LOCATION (build) so ort uses the system lib instead of
  #     downloading, and ORT_STRATEGY=system as a belt-and-suspenders, and
  #   * set ORT_DYLIB_PATH (runtime, via the gappsWrapperArgs) so the
  #     load-dynamic loader dlopen's the nixpkgs lib.
  # Whether ORT_LIB_LOCATION fully suppresses the download under the
  # load-dynamic + download-binaries combination for ort 2.0-rc.10 is the part
  # that NEEDS LINUX-SIDE VERIFICATION. If the build still tries to reach the
  # network, the fix is either a cargo overlay that drops `download-binaries`
  # from the engine manifest, or a fixed-output derivation that pins the pyke
  # onnxruntime tarball by hash. Flagged, not faked.
  # ===========================================================================

  description = "FileID — on-device AI file organizer (GTK4 + libadwaita)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/nixos-unstable";
    flake-utils.url = "github:numtide/flake-utils";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachDefaultSystem (system:
      let
        pkgs = import nixpkgs { inherit system; };
        # Repo root is two levels up from packaging/nix/.
        repoRoot = ../..;
      in {
        packages.fileid = pkgs.rustPlatform.buildRustPackage {
          pname = "fileid-linux";
          version = "0.1.0";

          src = repoRoot;
          # platforms/linux is the workspace; it path-depends on
          # ../../windows/src/engine, which is included in `src`.
          buildAndTestSubdir = "platforms/linux";

          cargoLock = {
            lockFile = ../../platforms/linux/Cargo.lock;
          };

          nativeBuildInputs = with pkgs; [
            pkg-config
            wrapGAppsHook4
            rustPlatform.bindgenHook
          ];

          buildInputs = with pkgs; [
            gtk4
            libadwaita
            glib
            gdk-pixbuf
            graphene
            cairo
            pango
            onnxruntime
            sqlite
          ];

          # Build the engine binary too (the app spawns it over stdio). It is a
          # path-dependency package, selected by name from the workspace graph.
          cargoBuildFlags = [ "-p" "fileid-linux" "-p" "fileid-engine" "--bin" "fileid-linux" "--bin" "FileIDEngine" ];

          # See the ONNX header above.
          ORT_STRATEGY = "system";
          ORT_LIB_LOCATION = "${pkgs.onnxruntime}/lib";

          # Runtime: make the GApps wrapper also export the dlopen path so the
          # load-dynamic loader finds nixpkgs' libonnxruntime.so.
          preFixup = ''
            gappsWrapperArgs+=(
              --set ORT_DYLIB_PATH "${pkgs.onnxruntime}/lib/libonnxruntime.so"
            )
          '';

          # The app's behavioural tests need a display + models; keep the
          # sandboxed build to a compile + the model-free unit tests only.
          doCheck = false;

          postInstall = ''
            install -Dm644 platforms/linux/data/io.github.fileid.FileID.desktop \
              "$out/share/applications/io.github.fileid.FileID.desktop"
            install -Dm644 platforms/linux/data/io.github.fileid.FileID.metainfo.xml \
              "$out/share/metainfo/io.github.fileid.FileID.metainfo.xml"
            install -Dm644 platforms/linux/data/io.github.fileid.FileID.svg \
              "$out/share/icons/hicolor/scalable/apps/io.github.fileid.FileID.svg"
          '';

          meta = with pkgs.lib; {
            description = "On-device AI file organizer — tag, dedupe, restructure, rename, locally";
            homepage = "https://github.com/fileid/FileID";
            license = licenses.asl20;
            platforms = platforms.linux;
            mainProgram = "fileid-linux";
          };
        };

        packages.default = self.packages.${system}.fileid;

        apps.fileid = {
          type = "app";
          program = "${self.packages.${system}.fileid}/bin/fileid-linux";
        };
        apps.default = self.apps.${system}.fileid;
      });
}
