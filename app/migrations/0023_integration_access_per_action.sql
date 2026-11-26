-- What Singularr may do with an integration, action by action: one row per integration and
-- action. A row saved before this names no action (`*`). It stands for every action its type
-- declares. The start writes it again as one row per action, with the same value, author and
-- time. An action without a row is read.
ALTER TABLE integration_access ADD COLUMN action text NOT NULL DEFAULT '*';
ALTER TABLE integration_access DROP CONSTRAINT integration_access_pkey;
ALTER TABLE integration_access ADD PRIMARY KEY (integration, action);
ALTER TABLE integration_access ALTER COLUMN action DROP DEFAULT;
