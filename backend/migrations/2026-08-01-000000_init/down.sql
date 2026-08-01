-- Reverse of up.sql. Dropped children-first so the FKs never block a drop.
DROP TABLE IF EXISTS agent_sessions;
DROP TABLE IF EXISTS agent_steps;
DROP TABLE IF EXISTS analysis_jobs;
DROP TABLE IF EXISTS foods;
DROP TABLE IF EXISTS item_corrections;
DROP TABLE IF EXISTS meal_items;
DROP TABLE IF EXISTS meals;
DROP TABLE IF EXISTS notification_groups;
DROP TABLE IF EXISTS thumbnails;
DROP TABLE IF EXISTS weight_logs;
DROP TABLE IF EXISTS user_profiles;
DROP TABLE IF EXISTS users;
