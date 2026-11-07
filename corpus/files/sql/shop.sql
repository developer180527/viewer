-- A small shop
CREATE TABLE products (
  id integer PRIMARY KEY,
  name text NOT NULL,
  price numeric(10,2)
);
CREATE INDEX products_name ON products (name);
INSERT INTO products (id, name, price) VALUES (1, 'Pen', 1.50), (2, 'It''s ink', NULL);
COPY orders (id, product_id) FROM stdin;
10	1
11	2
\.
SELECT name FROM products WHERE price > 1;
