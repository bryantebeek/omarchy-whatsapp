import QtQuick

// Covers the configuration surface the panel sets on the voice recorder.
// Recording behavior itself is covered through the service's voice recording
// test mode, so the transport calls stay inert.
QtObject {
  enum RecorderState { StoppedState, RecordingState, PausedState }
  enum RecorderError { NoError, ResourceError, FormatError }
  enum Quality {
    VeryLowQuality, LowQuality, NormalQuality, HighQuality, VeryHighQuality
  }
  enum EncodingMode {
    ConstantQualityEncoding, ConstantBitRateEncoding,
    AverageBitRateEncoding, TwoPassEncoding
  }

  property MediaFormat mediaFormat: MediaFormat {}
  property int quality: MediaRecorder.NormalQuality
  property int encodingMode: MediaRecorder.ConstantBitRateEncoding
  property int audioBitRate: 0
  property int audioChannelCount: 0
  property int audioSampleRate: 0
  property url outputLocation
  property int duration: 0
  property int recorderState: MediaRecorder.StoppedState
  property int error: MediaRecorder.NoError
  property string errorString: ""

  signal errorOccurred(int error, string errorString)

  function record() {}
  function stop() {}
}
