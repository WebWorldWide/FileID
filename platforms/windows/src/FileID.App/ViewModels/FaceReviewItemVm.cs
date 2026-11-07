using System.ComponentModel;
using FileID.IpcSchema;
using Microsoft.UI.Xaml.Media.Imaging;

namespace FileID.ViewModels;

internal sealed class FaceReviewItemVm : INotifyPropertyChanged
{
    public required FaceReviewItem Model { get; init; }

    public long FaceId => Model.FaceId;
    public long PersonId => Model.PersonId;
    public string PersonName => FaceReviewPolicy.Name(Model.PersonName);
    public string ReasonText => FaceReviewPolicy.Reason(Model);
    public string FileName => FaceReviewPolicy.FileName(Model.Path);
    public string RowLabel => FaceReviewPolicy.RowLabel(Model);
    public string FaceLabel => FaceReviewPolicy.FaceLabel(Model);
    public string PersonLabel => FaceReviewPolicy.PersonLabel(Model);
    public string AcceptLabel => FaceReviewPolicy.AcceptLabel(Model);
    public string RejectLabel => FaceReviewPolicy.RejectLabel(Model);
    public string IgnoreLabel => FaceReviewPolicy.IgnoreLabel;

    private BitmapImage? _faceImage;
    private bool _faceResolved;
    public BitmapImage? FaceImage => LoadCrop(Model.FaceId, ref _faceImage, ref _faceResolved);

    private BitmapImage? _personImage;
    private bool _personResolved;
    public BitmapImage? PersonImage => LoadCrop(Model.RepresentativeFaceId ?? 0, ref _personImage, ref _personResolved);

    private static BitmapImage? LoadCrop(long faceId, ref BitmapImage? cache, ref bool resolved)
    {
        if (resolved) return cache;
        resolved = true;
        if (faceId <= 0) return null;
        try
        {
            var path = PersonCluster.BuildCropPath(faceId);
            if (!System.IO.File.Exists(path)) return null;
            cache = new BitmapImage { DecodePixelWidth = 80, UriSource = new Uri(path) };
            return cache;
        }
        catch
        {
            return null;
        }
    }

    private bool _isBusy;
    public bool IsBusy
    {
        get => _isBusy;
        set
        {
            if (_isBusy == value) return;
            _isBusy = value;
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(IsBusy)));
            PropertyChanged?.Invoke(this, new PropertyChangedEventArgs(nameof(ActionsEnabled)));
        }
    }

    public bool ActionsEnabled => !_isBusy;

    public event PropertyChangedEventHandler? PropertyChanged;
}
