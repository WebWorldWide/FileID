{
  # Reproducible NixOS / Nix build of the FileID Linux app. Input revisions
  # are pinned below; dependency refreshes are intentional source edits.
  #
  #   nix build  ./packaging/nix#fileid
  #   nix run    ./packaging/nix#fileid
  #
  # Builds the GTK4 workspace (platforms/linux) plus the engine binary it
  # spawns, with gtk4 + libadwaita + onnxruntime from nixpkgs and wrapGAppsHook4
  # for the GTK/GdkPixbuf/GSettings runtime environment.
  #
  # ============================ ONNX RUNTIME — RISKIEST PART ==================
  # Portable Linux builds statically link `download-binaries`, but the Nix
  # sandbox has no network. Point ort-sys at nixpkgs' shared ONNX Runtime and
  # prefer dynamic linker selection instead.
  # We therefore:
  #   * put nixpkgs `onnxruntime` in buildInputs,
  #   * ORT_LIB_LOCATION suppresses the `download-binaries` fetch, and
  #   * ORT_PREFER_DYNAMIC_LINK selects nixpkgs' libonnxruntime.so.
  # ===========================================================================

  description = "FileID — on-device AI file organizer (GTK4 + libadwaita)";

  inputs = {
    nixpkgs.url = "github:NixOS/nixpkgs/0bb7ec54c8483066ec9d7720e780a5caa71f8612";
    flake-utils.url = "github:numtide/flake-utils/11707dc2f618dd54ca8739b309ec4fc024de578b";
  };

  outputs = { self, nixpkgs, flake-utils }:
    flake-utils.lib.eachSystem [ "x86_64-linux" "aarch64-linux" ] (system:
      let
        pkgs = import nixpkgs { inherit system; };
        # Repo root is two levels up from packaging/nix/.
        repoRoot = ../..;
      in {
        packages.fileid = pkgs.rustPlatform.buildRustPackage {
          pname = "fileid-linux";
          version = "0.1.1";

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
          ORT_LIB_LOCATION = "${pkgs.onnxruntime}/lib";
          ORT_PREFER_DYNAMIC_LINK = "1";

          # GTK interaction tests need a display. Exercise the headless Linux
          # app suite here; the engine's standalone dev-dependency graph is
          # validated in engine CI rather than mixed into this lockfile vendor.
          doCheck = true;
          checkPhase = ''
            runHook preCheck
            cargo test --frozen -p fileid-linux
            runHook postCheck
          '';

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
            homepage = "https://github.com/AdamNolle/FileID";
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
