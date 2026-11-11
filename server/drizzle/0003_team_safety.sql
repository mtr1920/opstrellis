ALTER TABLE "org_library" ADD COLUMN "key" text;--> statement-breakpoint
ALTER TABLE "orgs" ADD COLUMN "tombstone_floor" bigint DEFAULT 0 NOT NULL;--> statement-breakpoint
-- Hand-written: backfill the key of live entries the way the server computes it (NFKC, lowercase).
UPDATE "org_library" SET "key" = lower(normalize(btrim("data" ->> CASE "kind" WHEN 'dictionary' THEN 'phrase' WHEN 'snippet' THEN 'trigger' ELSE 'name' END), NFKC))
  WHERE NOT "deleted";--> statement-breakpoint
CREATE UNIQUE INDEX "org_library_live_key_idx" ON "org_library" USING btree ("org_id","kind","key") WHERE not "org_library"."deleted";--> statement-breakpoint
-- Hand-written (drizzle-kit does not model triggers): a team that still has active members but no
-- active admin, because its admin's user row was deleted or disabled, gets its longest-standing
-- active member as admin.
CREATE FUNCTION "org_ensure_active_admin"(p_org uuid) RETURNS void LANGUAGE plpgsql AS $$
BEGIN
  -- Same lock as every team write, so this cannot race a concurrent promotion or demotion.
  PERFORM 1 FROM "orgs" WHERE "id" = p_org FOR UPDATE;
  IF NOT EXISTS (
    SELECT 1 FROM "org_members" m JOIN "users" u ON u."id" = m."user_id"
    WHERE m."org_id" = p_org AND m."role" = 'admin' AND NOT u."disabled"
  ) THEN
    UPDATE "org_members" SET "role" = 'admin' WHERE "user_id" = (
      SELECT m."user_id" FROM "org_members" m JOIN "users" u ON u."id" = m."user_id"
      WHERE m."org_id" = p_org AND NOT u."disabled"
      ORDER BY m."created_at", m."user_id" LIMIT 1
    );
  END IF;
END $$;
--> statement-breakpoint
CREATE FUNCTION "org_members_keep_admin"() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  IF OLD."role" = 'admin' THEN PERFORM "org_ensure_active_admin"(OLD."org_id"); END IF;
  RETURN NULL;
END $$;
--> statement-breakpoint
-- Named to fire after org_members_drop_empty_org (triggers fire in name order).
CREATE TRIGGER "org_members_keep_admin" AFTER DELETE ON "org_members" FOR EACH ROW EXECUTE FUNCTION "org_members_keep_admin"();
--> statement-breakpoint
CREATE FUNCTION "users_disabled_keep_team_admin"() RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
  PERFORM "org_ensure_active_admin"(m."org_id") FROM "org_members" m WHERE m."user_id" = NEW."id" AND m."role" = 'admin';
  RETURN NULL;
END $$;
--> statement-breakpoint
CREATE TRIGGER "users_disabled_keep_team_admin" AFTER UPDATE OF "disabled" ON "users" FOR EACH ROW
  WHEN (NEW."disabled" AND NOT OLD."disabled") EXECUTE FUNCTION "users_disabled_keep_team_admin"();
