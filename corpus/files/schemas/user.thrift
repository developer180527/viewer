namespace java demo
struct User {
  1: required string name,
  2: optional i32 age = 0
}
service Users {
  User get(1: string name)
}
