import QtQuick

// Covers the container and codec surface the voice recorder configures.
QtObject {
  enum FileFormat { UnspecifiedFormat, Ogg }
  enum AudioCodec { UnspecifiedAudioCodec, Opus }

  property int fileFormat: MediaFormat.UnspecifiedFormat
  property int audioCodec: MediaFormat.UnspecifiedAudioCodec
}
