#!/usr/bin/env python3

import config_pb2
import google.protobuf.text_format as text_format
import sys

default_config_location = sys.argv[1]
provided_config_location = sys.argv[2]

default_config = None
provided_config = None

with open(default_config_location, 'rt') as default_config_raw:
    default_config = text_format.Parse(''.join(default_config_raw.readlines()), config_pb2.Config())

with open(provided_config_location, 'rt') as provided_config_raw:
    provided_config = text_format.Parse(''.join(provided_config_raw.readlines()), config_pb2.Config())

default_config.MergeFrom(provided_config)

print(text_format.MessageToString(default_config))
