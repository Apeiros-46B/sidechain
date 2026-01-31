{ config, lib, pkgs, ... }:

with lib;
let
  cfg = config.services.sidechain;
  args = [
    "-i" cfg.sourceDir
    "-o" cfg.destinationDir
    "-d" cfg.dbPath
    "-f" cfg.format
    "-b" (toString cfg.bitrate)
  ]
  ++ (concatMap (x: [ "-a" x ]) cfg.allowedExtensions)
  ++ (concatMap (x: [ "-x" x ]) cfg.ignoredExtensions)
  ++ (optional (cfg.maxThreads != null) [ "-t" (toString cfg.maxThreads) ])
  ++ (optional cfg.copy "--copy");
in {
  options.services.sidechain = {
    enable = mkEnableOption "Sidechain music mirror service";
    package = mkOption {
      type = types.package;
      default = pkgs.sidechain;
      description = "The sidechain package to use.";
    };
    user = mkOption {
      type = types.str;
      default = "sidechain";
      description = "User account under which to run the service.";
    };
    group = mkOption {
      type = types.str;
      default = "sidechain";
      description = "Group under which to run the service.";
    };
    interval = mkOption {
      type = types.str;
      default = "daily";
      example = "02:00";
      description = "systemd calendar interval for the timer.";
    };
    logLevel = mkOption {
      type = types.str;
      default = "info";
      description = "RUST_LOG environment variable level.";
    };
    sourceDir = mkOption {
      type = types.path;
      description = "Path to the lossless source directory.";
    };
    destinationDir = mkOption {
      type = types.path;
      description = "Path to the destination directory.";
    };
    dbPath = mkOption {
      type = types.path;
      default = "/var/lib/sidechain/sidechain.db";
      description = "Path to the SQLite database file.";
    };
    allowedExtensions = mkOption {
      type = types.listOf types.str;
      default = [ "wav" "flac" "alac" "aiff" ];
      description = "List of extensions to transcode.";
    };
    ignoredExtensions = mkOption {
      type = types.listOf types.str;
      default = [];
      description = "List of extensions to ignore completely.";
    };
    format = mkOption {
      type = types.str;
      default = "opus";
      description = "Output format (ffmpeg encoder file extension)";
    };
    bitrate = mkOption {
      type = types.int;
      default = 160;
      description = "Bitrate in kbps.";
    };
    maxThreads = mkOption {
      type = types.nullOr types.int;
      default = null;
      description = "Maximum worker threads. Null uses core count minus 1.";
    };
    copy = mkOption {
      type = types.bool;
      default = false;
      description = "Copy files instead of hardlinking (useful for FAT32 destinations).";
    };
    nice = mkOption {
      type = types.int;
      default = 0;
      description = "Niceness value set in systemd service.";
    };
  };

  config = mkIf cfg.enable {
    users.users = mkIf (cfg.user == "sidechain") {
      sidechain = {
        isSystemUser = true;
        group = cfg.group;
        description = "Sidechain service user";
      };
    };
    users.groups = mkIf (cfg.group == "sidechain") {
      sidechain = {};
    };

    systemd.tmpfiles.settings."10-sidechain".${dirOf cfg.dbPath}.d = {
      mode = "0750";
      user = cfg.user;
      group = cfg.group;
    };

    systemd.services.sidechain = {
      description = "Sidechain music mirror";
      environment.RUST_LOG = cfg.logLevel;

      serviceConfig = {
        Type = "oneshot";
        ExecStart = "${cfg.package}/bin/sidechain ${concatStringsSep " " (flatten args)}";
				Nice = cfg.nice;
        User = cfg.user;
        Group = cfg.group;
      };
    };

    systemd.timers.sidechain = {
      wantedBy = [ "timers.target" ];
      timerConfig = {
        OnCalendar = cfg.interval;
        Persistent = true; # run immediately if missed while off
        Unit = "sidechain.service";
      };
    };
  };
}
