terraform {
  required_version = ">= 1.5"
}

resource "aws_s3_bucket" "logs" {
  bucket = "my-logs"
  tags = {
    env = "prod"
  }
  versioning {
    enabled = true
  }
}

variable "region" {
  default = "eu-west-1"
}
