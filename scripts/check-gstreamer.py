"""Headless CI codec preflight. Never opens a display, microphone or sound card."""
import argparse
import subprocess

parser = argparse.ArgumentParser()
parser.add_argument("--encoders", action="store_true")
args = parser.parse_args()
elements = "appsrc appsink audiotestsrc videoconvert audioconvert audioresample h264parse h265parse avdec_h264 avdec_h265 aacparse avdec_aac opusdec".split()
if args.encoders:
    elements += "x264enc x265enc avenc_aac opusenc".split()
for element in elements:
    subprocess.run(["gst-inspect-1.0", element], check=True, stdout=subprocess.DEVNULL)
print("GStreamer elements available:", ", ".join(elements))
