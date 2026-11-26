-- The actions each integration's type declared at the last start. The next start compares them
-- with the type's actions then. An `act` of the environment or the instance file covers every
-- action of the type. That includes an action a later version adds. The start names each such
-- action in a warning.
CREATE TABLE integration_actions (
    integration text NOT NULL,
    action text NOT NULL,
    PRIMARY KEY (integration, action)
);
