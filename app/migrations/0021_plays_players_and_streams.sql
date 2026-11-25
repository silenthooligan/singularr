-- What every play records of its player and its stream, whichever media server it was on: the
-- server's kind, the versions of the player app and of its platform, the device's own id,
-- whether the connection was encrypted or relayed, the bandwidth, the codecs, audio channels and
-- containers of the file and of what was sent, and why the server transcoded.
ALTER TABLE plays
    ADD COLUMN server_kind           text,
    ADD COLUMN product_version       text,
    ADD COLUMN platform_version      text,
    ADD COLUMN device_id             text,
    ADD COLUMN secure                boolean,
    ADD COLUMN relayed               boolean,
    ADD COLUMN bandwidth             bigint,
    ADD COLUMN source_video_codec    text,
    ADD COLUMN source_audio_codec    text,
    ADD COLUMN source_audio_channels integer,
    ADD COLUMN source_container      text,
    ADD COLUMN stream_video_codec    text,
    ADD COLUMN stream_audio_codec    text,
    ADD COLUMN stream_audio_channels integer,
    ADD COLUMN stream_container      text,
    ADD COLUMN transcode_reasons     text[] NOT NULL DEFAULT '{}';

-- The plays Tautulli told of hold its row in `raw`: its device id, whether the connection was
-- secure or relayed, and the address, player, platform, app and delivery where the play does
-- not hold them yet. Tautulli reads Plex.
UPDATE plays SET
    server_kind = COALESCE(server_kind, 'plex'),
    device_id = COALESCE(device_id, NULLIF(raw #>> '{tautulli,machine_id}', '')),
    secure = COALESCE(secure, CASE lower(raw #>> '{tautulli,secure}')
                                  WHEN '1' THEN true WHEN 'true' THEN true
                                  WHEN '0' THEN false WHEN 'false' THEN false END),
    relayed = COALESCE(relayed, CASE lower(raw #>> '{tautulli,relayed}')
                                    WHEN '1' THEN true WHEN 'true' THEN true
                                    WHEN '0' THEN false WHEN 'false' THEN false END),
    address = COALESCE(address, NULLIF(raw #>> '{tautulli,ip_address}', '')),
    player = COALESCE(player, NULLIF(raw #>> '{tautulli,player}', '')),
    platform = COALESCE(platform, NULLIF(raw #>> '{tautulli,platform}', '')),
    product = COALESCE(product, NULLIF(raw #>> '{tautulli,product}', '')),
    decision = COALESCE(decision, CASE lower(raw #>> '{tautulli,transcode_decision}')
                                      WHEN 'direct play' THEN 'direct play'
                                      WHEN 'copy' THEN 'direct stream'
                                      WHEN 'transcode' THEN 'transcode' END)
WHERE raw ? 'tautulli';

-- A Plex server's own history and its sessions (a session record with a Player) are Plex's.
UPDATE plays SET server_kind = 'plex'
WHERE server_kind IS NULL AND (raw ? 'server' OR raw #> '{live,Player}' IS NOT NULL);

CREATE INDEX plays_address ON plays (account_id, address) WHERE address IS NOT NULL;
CREATE INDEX plays_server ON plays (server, started_at DESC);
