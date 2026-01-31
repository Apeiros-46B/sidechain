{ lib
, rustPlatform
, makeWrapper
, libsrc ? ./.
, pkg-config
, sqlite
, ffmpeg
}:

let
  manifest = (lib.importTOML ./Cargo.toml).package;
in rustPlatform.buildRustPackage {
  pname = manifest.name;
  version = manifest.version;
  meta = with lib; {
    description = "Tool that makes a lossy mirror of your lossless music collection.";
    homepage = "https://github.com/Apeiros-46B/sidechain";
    license = licenses.unlicense;
    maintainers = [];
  };

  nativeBuildInputs = [ pkg-config makeWrapper ];
  buildInputs = [ sqlite ];

  src = lib.cleanSource libsrc;
  cargoLock.lockFile = ./Cargo.lock;

  postInstall = ''
    wrapProgram $out/bin/${manifest.name} \
      --prefix PATH : ${lib.makeBinPath [ ffmpeg ]}
  '';
}
