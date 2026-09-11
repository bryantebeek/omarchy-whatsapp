import QtQuick

// Deterministic stand-in for QtMultimedia playback: transport calls flip the
// reported state synchronously and no decoder backend ever runs, so viewer
// and voice tests stay hermetic and warning-free.
QtObject {
  enum PlaybackState { StoppedState, PlayingState, PausedState }
  enum MediaStatus {
    NoMedia, LoadingMedia, LoadedMedia, StalledMedia, BufferingMedia,
    BufferedMedia, EndOfMedia, InvalidMedia
  }
  enum PlayerError {
    NoError, ResourceError, FormatError, NetworkError, AccessDeniedError
  }
  enum Loops { Once, Infinite }

  property url source
  property int playbackState: MediaPlayer.StoppedState
  property int mediaStatus: MediaPlayer.NoMedia
  property int error: MediaPlayer.NoError
  property string errorString: ""
  property int loops: MediaPlayer.Once
  property var audioOutput: null
  property var videoOutput: null
  property int position: 0
  property int duration: 0

  function play() { playbackState = MediaPlayer.PlayingState }
  function pause() { playbackState = MediaPlayer.PausedState }
  function stop() { playbackState = MediaPlayer.StoppedState; position = 0 }
}
