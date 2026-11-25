-- What Marquee may do with an integration, as the owner saved it in the interface
-- (PATCH /api/v1/integrations/{id}): `read`, or `act` (read it, and do what its adapter lists
-- as actions). The engines module owns this table. An integration without a row is read,
-- unless the environment or the instance file says otherwise: those fix the access, and a
-- value they fix is never stored here. The journal holds every change as an
-- `integration_access_changed` event.
CREATE TABLE integration_access (
    integration text PRIMARY KEY,
    access      text NOT NULL CHECK (access IN ('read', 'act')),
    saved_by    text NOT NULL,
    saved_at    timestamptz NOT NULL DEFAULT now()
);
