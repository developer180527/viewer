package app

// settings
#Config: {
  port: int & >0
  host: string | *"localhost"
}
config: #Config & {
  port: 8080
}
