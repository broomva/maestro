CREATE TABLE `event` (
	`seq` integer PRIMARY KEY AUTOINCREMENT NOT NULL,
	`session_id` text,
	`ts` integer NOT NULL,
	`actor` text NOT NULL,
	`type` text NOT NULL,
	`payload_json` text
);
--> statement-breakpoint
CREATE INDEX `event_session_seq_idx` ON `event` (`session_id`,`seq`);--> statement-breakpoint
CREATE TABLE `gate` (
	`id` text PRIMARY KEY NOT NULL,
	`session_id` text NOT NULL,
	`kind` text NOT NULL,
	`proposal_json` text,
	`verdict` text,
	`decided_by` text,
	`opened_at` integer NOT NULL,
	`decided_at` integer,
	`updated_at` integer NOT NULL,
	`deleted_at` integer
);
--> statement-breakpoint
CREATE TABLE `lease` (
	`key` text PRIMARY KEY NOT NULL,
	`holder` text NOT NULL,
	`acquired_at` integer NOT NULL,
	`expires_at` integer NOT NULL
);
--> statement-breakpoint
CREATE TABLE `node` (
	`id` text PRIMARY KEY NOT NULL,
	`path` text NOT NULL,
	`parent_id` text,
	`kind` text NOT NULL,
	`state` text NOT NULL,
	`owner` text,
	`gate` text DEFAULT 'human' NOT NULL,
	`budget_json` text,
	`done_json` text,
	`title` text,
	`created_at` integer NOT NULL,
	`updated_at` integer NOT NULL,
	`deleted_at` integer
);
--> statement-breakpoint
CREATE UNIQUE INDEX `node_path_live_unique` ON `node` (`path`) WHERE "node"."deleted_at" IS NULL;--> statement-breakpoint
CREATE INDEX `node_parent_idx` ON `node` (`parent_id`);--> statement-breakpoint
CREATE INDEX `node_state_idx` ON `node` (`state`);--> statement-breakpoint
CREATE TABLE `run_budget` (
	`session_id` text PRIMARY KEY NOT NULL,
	`spent_usd` real DEFAULT 0 NOT NULL,
	`iterations` integer DEFAULT 0 NOT NULL,
	`last_call_at` integer
);
--> statement-breakpoint
CREATE TABLE `scan_cursor` (
	`path` text PRIMARY KEY NOT NULL,
	`byte_offset` integer NOT NULL,
	`last_seq` integer NOT NULL,
	`updated_at` integer NOT NULL
);
--> statement-breakpoint
CREATE TABLE `schedule` (
	`id` text PRIMARY KEY NOT NULL,
	`node_id` text NOT NULL,
	`trigger_kind` text NOT NULL,
	`spec` text NOT NULL,
	`next_fire_at` integer,
	`enabled` integer DEFAULT true NOT NULL,
	`updated_at` integer NOT NULL,
	`deleted_at` integer
);
--> statement-breakpoint
CREATE INDEX `schedule_enabled_idx` ON `schedule` (`enabled`);--> statement-breakpoint
CREATE TABLE `session` (
	`id` text PRIMARY KEY NOT NULL,
	`node_id` text NOT NULL,
	`branch` text NOT NULL,
	`status` text NOT NULL,
	`started_at` integer NOT NULL,
	`ended_at` integer,
	`diffstat_json` text,
	`updated_at` integer NOT NULL,
	`deleted_at` integer
);
--> statement-breakpoint
CREATE INDEX `session_node_idx` ON `session` (`node_id`);