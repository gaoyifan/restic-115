{self}: {
  config,
  lib,
  pkgs,
  ...
}: let
  cfg = config.services.restic-115;
in {
  options.services.restic-115 = {
    enable = lib.mkEnableOption "restic-115 backend";

    package = lib.mkOption {
      type = lib.types.package;
      default = self.packages.${pkgs.stdenv.hostPlatform.system}.default;
      description = "restic-115 package to run.";
    };

    environmentFile = lib.mkOption {
      type = lib.types.nullOr lib.types.externalPath;
      default = null;
      description = "Environment file containing the 115 access and refresh tokens.";
    };

    cacheDirectory = lib.mkOption {
      type = lib.types.str;
      default = "/var/lib/restic-115";
      description = "Directory containing the restic-115 SQLite cache.";
    };

    repositoryPath = lib.mkOption {
      type = lib.types.str;
      default = "/restic-backup";
      description = "Root folder path on 115 for the repository.";
    };

    listenAddress = lib.mkOption {
      type = lib.types.str;
      default = "127.0.0.1";
      description = "Address on which restic-115 listens.";
    };

    listenPort = lib.mkOption {
      type = lib.types.port;
      default = 8000;
      description = "Port on which restic-115 listens.";
    };

    forceCacheRebuild = lib.mkOption {
      type = lib.types.bool;
      default = false;
      description = "Force rebuilding the restic-115 cache on startup.";
    };

    user = lib.mkOption {
      type = lib.types.str;
      default = "root";
      description = "User running restic-115.";
    };

    group = lib.mkOption {
      type = lib.types.str;
      default = "root";
      description = "Group running restic-115.";
    };
  };

  config = lib.mkIf cfg.enable {
    systemd.tmpfiles.rules = [
      "d ${cfg.cacheDirectory} 0750 ${cfg.user} ${cfg.group} -"
    ];

    systemd.services.restic-115 = {
      description = "Restic REST backend for 115 Open Platform storage";
      wantedBy = lib.mkDefault ["multi-user.target"];
      after = ["network-online.target"];
      wants = ["network-online.target"];
      unitConfig.RequiresMountsFor = [cfg.cacheDirectory];
      serviceConfig =
        {
          ExecStart = lib.getExe cfg.package;
          User = cfg.user;
          Group = cfg.group;
          Restart = "on-failure";
          RestartSec = "5s";
        }
        // lib.optionalAttrs (cfg.environmentFile != null) {
          EnvironmentFile = cfg.environmentFile;
        };
      environment = {
        LISTEN_ADDR = cfg.listenAddress;
        LISTEN_PORT = toString cfg.listenPort;
        OPEN115_REPO_PATH = cfg.repositoryPath;
        OPEN115_FORCE_CACHE_REBUILD = lib.boolToString cfg.forceCacheRebuild;
        DB_PATH = "${cfg.cacheDirectory}/cache-115.db";
      };
    };
  };
}
